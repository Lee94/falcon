local tern = require("tern")
local ui = tern.ui
local launch = require("./launch")

local function with(node, props)
	node.p = node.p or {}
	for key, value in props do
		node.p[key] = value
	end
	return node
end

local function key(prefix, id)
	return prefix .. tern.base64.encode(tostring(id), { url = true })
end

local function text(value)
	return if type(value) == "string" and value:find("%S") then value else nil
end

local function line(spans, id, role)
	return with(ui.text(spans), { key = id, role = role, wrap = "char" })
end

local function running(pane)
	for _, candidate in tern.pane.list() do
		if candidate.pane == pane then
			return true
		end
	end
	return false
end

local function archived(project)
	return project.worktree and project.worktree.archivedAt and project.worktree.archivedAt ~= 0
end

local function disclosure(state, id, label, children, role)
	return with(ui.section(label, children), {
		key = id,
		role = role,
		collapsible = true,
		collapsed = state.folds[id] == true,
	})
end

local function sessionTitle(session, project)
	return text(session.name) or text(session.title) or text(session.agent) or (project and text(project.shell)) or "shell"
end

local function projectTitle(project)
	local name = text(project.name) or project.id
	if project.worktree then
		name ..= " · " .. (text(project.worktree.branch) or "detached")
	end
	return name
end

local function helper(config, args, done)
	local argv = { config.bridge, "--config", config.config }
	for _, arg in args do table.insert(argv, arg) end
	local ok, err = pcall(tern.process.run, argv, { timeout_ms = 60000 }, function(result)
		if result.timed_out then done(nil, "Falcon 操作超时（60 秒）"); return end
		if result.status ~= 0 then
			done(nil, text(result.stderr) or ("Falcon helper 退出码 " .. result.status))
			return
		end
		local decoded, value = pcall(tern.json.decode, result.stdout)
		if not decoded or type(value) ~= "table" then done(nil, "Falcon 返回了无效的操作结果"); return end
		done(value, nil)
	end)
	if not ok then done(nil, tostring(err)) end
end

local function detectAgents(state, snapshot, config, cx)
	local covered = {}
	local function probe(owners)
		if #owners == 0 then return end
		for _, owner in owners do covered[owner] = true end
		helper(config, { "agents", owners[1] }, function(agents, err)
			if state.snapshot ~= snapshot or state.config ~= config or not running(cx.pane) then return end
			for _, owner in owners do
				state.agents[owner] = agents
				state.agentErrors[owner] = err
			end
			cx:render()
		end)
	end
	-- Projects in one server group share the same login environment. Probe once
	-- per host without delaying the project tree or ordinary-terminal creation.
	for _, group in snapshot.groups do
		local owners = {}
		if group.standaloneId then table.insert(owners, group.standaloneId) end
		for _, folder in group.folders do
			table.insert(owners, folder.project.id)
			for _, project in folder.worktrees do table.insert(owners, project.id) end
		end
		probe(owners)
	end
	for _, project in snapshot.projects do
		if not covered[project.id] then probe({ project.id }) end
	end
end

local function creationControls(state, owner)
	local pending = state.creating[owner]
	local items = {
		{ id = "create-terminal:" .. owner, label = "普通终端" },
	}
	if table.find(state.agents[owner] or {}, "omp") then
		table.insert(items, { id = "create-omp:" .. owner, label = "OMP" })
	end
	return ui.node("menu", {
		key = "create", role = "falcon.create",
		text = if pending then "正在创建…" else "新建会话",
		caret = true, size = "sm", variant = "dense",
		disabled = state.busy or pending ~= nil,
		items = items,
	})
end

local function sessionRows(state, projectId, project, context)
	local sessions = state.byProject[projectId] or {}
	local rows = {}
	if state.creatable[projectId] then
		table.insert(rows, creationControls(state, projectId))
		if state.agentErrors[projectId] then
			table.insert(rows, line({ ui.span("OMP 检测失败：" .. state.agentErrors[projectId], "warning") }, "agent-error", "falcon.status"))
		end
	end
	for _, session in sessions do
		local title = sessionTitle(session, project)
		local status = session.state or "unverified"
		local tone = if status == "active" then "success" elseif status == "dead" then "error" else "warning"
		local details = {
			ui.span(status, tone),
			ui.span(if session.durable then " · 持久会话" else " · 非持久会话", "muted"),
		}
		if session.deadReason then
			table.insert(details, ui.span(" · " .. session.deadReason, "error"))
		end
		if session.programStatus then
			local program = session.programStatus
			table.insert(details, ui.span(" · " .. (text(program.title) or text(program.message) or program.state), "info"))
		end
		local attachTitle = context .. " · " .. title
		local children = {
			line({ ui.span(title, "strong") }, "title", "falcon.session-title"),
			line(details, "status", "falcon.status"),
		}
		local props = {
			key = key("session-", session.id),
			role = "falcon.session",
			title = attachTitle,
		}
		if status ~= "dead" and state.config and not state.opening[session.id] then
			props.actions = { click = "enter=" .. session.id }
		end
		table.insert(rows, with(ui.col(children), props))
	end
	if #sessions == 0 then
		table.insert(rows, line({ ui.span("暂无会话", "muted") }, "empty", "falcon.empty"))
	end
	return rows
end

local function projectNode(state, project, hostName, worktrees)
	local children = {}
	local remote = project.type == "ssh"
	local path = text(project.workingDir)
	table.insert(children, line({
		ui.span(if remote then "远端目录  " else "本机目录  ", "muted"),
		ui.span(path or (if project.multi then "多仓库虚拟目录（成员目录见下）" else "未指定（宿主登录目录）"), "path"),
	}, "directory", "falcon.directory"))
	if project.worktree then
		table.insert(children, line({
			ui.span("分支  ", "muted"), ui.span(text(project.worktree.branch) or "detached", "mono"),
			ui.span(if archived(project) then " · 已存档" else "", "warning"),
		}, "branch", "falcon.branch"))
	end
	if project.multi then
		local members = {}
		for index, member in project.multi.repos do
			table.insert(members, line({ ui.span(member.dir, "path") }, key("repo-", member.dir) .. "-" .. index, "falcon.directory"))
		end
		if #members == 0 then
			table.insert(members, line({ ui.span("暂无成员仓库", "muted") }, "empty", "falcon.empty"))
		end
		table.insert(children, disclosure(state, key("repos-", project.id), "成员仓库 · " .. #project.multi.repos, members, "falcon.repos"))
	end
	local context = hostName .. " / " .. projectTitle(project)
	if path then
		context ..= " · " .. path
	end
	table.insert(children, disclosure(state, key("sessions-", project.id), "会话 · " .. #(state.byProject[project.id] or {}), sessionRows(state, project.id, project, context), "falcon.sessions"))
	if worktrees and #worktrees > 0 then
		local checkouts = {}
		local hidden = 0
		for _, checkout in worktrees do
			if state.showArchived or not archived(checkout) then
				table.insert(checkouts, projectNode(state, checkout, hostName, nil))
			else
				hidden += 1
			end
		end
		if hidden > 0 then
			table.insert(checkouts, line({ ui.span(tostring(hidden) .. " 个存档附属项目已隐藏", "muted") }, "archived-hidden", "falcon.empty"))
		end
		table.insert(children, disclosure(state, key("worktrees-", project.id), "附属项目 · " .. #worktrees, checkouts, "falcon.worktrees"))
	end
	local label = projectTitle(project)
	if archived(project) then
		label ..= " · 已存档"
	end
	if project.multi then
		label ..= " · 多仓库"
	end
	return disclosure(state, key("project-", project.id), label, children, "falcon.project")
end

local function tree(state)
	local hosts = {}
	local seen = {}
	for _, group in state.snapshot.groups do
		local children = {}
		local localHost = group.key == "s:local"
		table.insert(children, line({
			ui.span(if localHost then "Falcon 后端本机" else "远端主机", "muted"),
			ui.span(if text(group.conn) then " · " .. group.conn else "", "mono"),
		}, "connection", "falcon.connection"))
		for _, folder in group.folders do
			seen[folder.project.id] = true
			for _, checkout in folder.worktrees do
				seen[checkout.id] = true
			end
			if state.showArchived or not archived(folder.project) then
				table.insert(children, projectNode(state, folder.project, group.name, folder.worktrees))
			end
		end
		if group.standaloneId then
			seen[group.standaloneId] = true
			local sessions = state.byProject[group.standaloneId] or {}
			table.insert(children, disclosure(state, key("standalone-", group.standaloneId), "独立会话 · " .. #sessions,
				sessionRows(state, group.standaloneId, nil, group.name .. " / 独立会话"), "falcon.sessions"))
		end
		if #children == 1 then
			table.insert(children, line({ ui.span("暂无项目", "muted") }, "empty", "falcon.empty"))
		end
		table.insert(hosts, disclosure(state, key("host-", group.key), group.name .. (if localHost then "" else " · SSH"), children, "falcon.host"))
	end
	-- Preserve projects that the server grouping cannot place (for example a deleted
	-- saved SSH host); never merge their path namespace with a different host.
	for _, project in state.snapshot.projects do
		if not seen[project.id] then
			seen[project.id] = true
			if state.showArchived or not archived(project) then
				local namespace = if project.type == "ssh" then "未分组远端主机 · " .. (project.hostId or project.id) else "未分组本机"
				table.insert(hosts, disclosure(state, key("unplaced-", project.id), namespace, {
					projectNode(state, project, namespace, nil),
				}, "falcon.host"))
			end
		end
	end
	local missing = {}
	for projectId, sessions in state.byProject do
		if not seen[projectId] then
			table.insert(missing, { id = projectId, sessions = sessions })
		end
	end
	table.sort(missing, function(a, b) return a.id < b.id end)
	for _, entry in missing do
		local session = entry.sessions[1]
		local label = (text(session.projectName) or "项目已删除") .. " · " .. entry.id
		table.insert(hosts, disclosure(state, key("missing-", entry.id), label, {
			line({ ui.span("原项目目录不可用 · " .. (session.projectType or "unknown"), "muted") }, "directory", "falcon.directory"),
			unpack(sessionRows(state, entry.id, nil, label)),
		}, "falcon.project"))
	end
	return with(ui.col(hosts), { key = "tree", role = "falcon.tree" })
end

tern.block.define("projects", {
	font = "sans",
	init = function(cx, _args, saved)
		local state = {
			folds = if type(saved) == "table" and type(saved.folds) == "table" then saved.folds else {},
			showArchived = type(saved) == "table" and saved.showArchived == true,
			busy = false,
			snapshot = nil,
			byProject = {},
			config = nil,
			error = nil,
			opening = {},
			creating = {},
			creatable = {},
			agents = {},
			agentErrors = {},
		}
		local function fail(message)
			state.busy = false
			state.snapshot = nil
			state.config = nil
			state.error = message
			cx:render()
		end
		-- Each helper invocation authenticates a new client; refresh only on user request.
		local function refresh()
			if state.busy or next(state.creating) ~= nil or not running(cx.pane) then
				return
			end
			state.busy = true
			local ok, config = pcall(launch.config)
			if not ok then
				fail(tostring(config))
				return
			end
			local spawned, err = pcall(function()
				tern.process.run({ config.bridge, "--config", config.config, "snapshot" }, { timeout_ms = 10000 }, function(result)
					if not running(cx.pane) then
						return
					end
					if result.timed_out then
						fail("Falcon snapshot 超时（10 秒）")
						return
					end
					if result.status ~= 0 then
						fail(text(result.stderr) or ("Falcon helper 退出码 " .. result.status))
						return
					end
					local decoded, snapshot = pcall(function()
						local value = tern.json.decode(result.stdout)
						assert(type(value) == "table" and type(value.groups) == "table" and type(value.projects) == "table"
							and type(value.sessions) == "table" and type(value.hosts) == "table", "Invalid Falcon snapshot shape")
						return value
					end)
					if not decoded then
						fail("Falcon snapshot JSON 无效：" .. tostring(snapshot))
						return
					end
					state.byProject = {}
					state.creatable = {}
					state.agents = {}
					state.agentErrors = {}
					for _, project in snapshot.projects do
						if not archived(project) then state.creatable[project.id] = true end
					end
					for _, group in snapshot.groups do
						if group.standaloneId then state.creatable[group.standaloneId] = true end
					end
					for _, session in snapshot.sessions do
						local entries = state.byProject[session.projectId]
						if not entries then
							entries = {}
							state.byProject[session.projectId] = entries
						end
						table.insert(entries, session)
					end
					state.snapshot = snapshot
					state.config = config
					state.error = nil
					state.busy = false
					cx:render()
					detectAgents(state, snapshot, config, cx)
				end)
			end)
			if not spawned then
				fail(tostring(err))
			end
		end
		state.refresh = refresh
		refresh()
		return state
	end,
	title = function() return "Falcon Projects" end,
	view = function(state, _cx)
		local children = {
			line({ ui.span("Falcon", "title"), ui.span(" · 项目与会话", "muted") }, "heading", "falcon.heading"),
			with(ui.row({
				ui.button(if state.error then "重试" else "刷新", { key = "refresh", busy = state.busy, actions = { click = "refresh" } }),
				ui.button(if state.showArchived then "隐藏存档" else "显示存档", { key = "archived", on = state.showArchived, actions = { click = "archived" } }),
			}), { key = "toolbar", wrap = true, role = "falcon.toolbar" }),
		}
		if state.error then
			table.insert(children, with(ui.card("无法读取 Falcon", {
				line({ ui.span(state.error, "error") }, "message", "falcon.error"),
				line({ ui.span("配置文件：", "muted"), ui.span(tern.plugin.data .. "/config.json", "path") }, "config", "falcon.directory"),
			}), { key = "error", tone = "error" }))
		elseif state.snapshot then
			table.insert(children, tree(state))
		else
			table.insert(children, with(ui.progress(nil, "正在读取 Falcon…"), { key = "loading" }))
		end
		return {
			main = ui.col(children),
			dock = ui.col({ line({ ui.span("刷新或按 R 更新列表 · Ctrl+\\ 断开视图 · 关闭视图保留 Falcon 会话", "muted") }, "help", "falcon.help") }),
		}
	end,
	event = function(state, event, cx)
		-- Menu rows send their id as `act`; button action parameters are not used.
		local createAction, createOwner = (event.act or ""):match("^(create%-%a+):(.+)$")
		if event.ev == "toggle" and type(event.key) == "string" and type(event.collapsed) == "boolean" then
			state.folds[event.key] = event.collapsed
		elseif event.ev == "action" and event.act == "refresh" then
			state.refresh()
		elseif event.ev == "action" and event.act == "archived" then
			state.showArchived = not state.showArchived
		elseif event.ev == "action" and (createAction == "create-terminal" or createAction == "create-omp") and state.config then
			local owner = createOwner
			local agent = if createAction == "create-omp" then "omp" else nil
			if not state.creatable[owner] or state.busy or state.creating[owner] then return end
			if agent and not table.find(state.agents[owner] or {}, agent) then return end
			state.creating[owner] = agent or "terminal"
			cx:render()
			local args = { "create", owner }
			if agent then table.insert(args, "--agent"); table.insert(args, agent) end
			helper(state.config, args, function(session, err)
				local function finish(message)
					state.creating[owner] = nil
					if running(cx.pane) then
						if message then cx:toast("error", "创建会话失败", message) end
						cx:render()
					end
				end
				if err then finish(err); return end
				if type(session.id) ~= "string" or session.id == "" or session.projectId ~= owner then
					finish("Falcon 返回了无效的会话信息")
					return
				end
				local sessions = state.byProject[owner]
				if not sessions then sessions = {}; state.byProject[owner] = sessions end
				table.insert(sessions, session)
				table.insert(state.snapshot.sessions, session)
				state.folds[key("sessions-", owner)] = false
				if not running(cx.pane) then finish(nil); return end
				cx:render()
				local function opened(message)
					state.creating[owner] = nil
					if running(cx.pane) then
						if message then cx:toast("error", "会话已创建，接入失败", message) end
						cx:render()
					end
				end
				local ok, openError = pcall(launch.open, state.config, cx.pane, session.id, opened)
				if not ok then opened(tostring(openError)) end
			end)
		elseif event.ev == "action" and event.act == "enter" and state.config then
			local session = nil
			for _, candidate in state.snapshot.sessions do
				if candidate.id == event.value then
					session = candidate
					break
				end
			end
			if not session or session.state == "dead" or state.opening[session.id] then
				return
			end
			state.opening[session.id] = true
			local function done(message)
				state.opening[session.id] = nil
				if running(cx.pane) then
					if message then
						cx:toast("error", "会话操作失败", message)
					end
					cx:render()
				end
			end
			local ok, err = pcall(launch.open, state.config, cx.pane, session.id, done)
			if not ok then
				done(tostring(err))
			end
		end
	end,
	key = function(state, pressed, _cx)
		if pressed.name == "r" and not pressed.ctrl and not pressed.meta and not pressed.alt then
			state.refresh()
		elseif pressed.name == "a" and not pressed.ctrl and not pressed.meta and not pressed.alt then
			state.showArchived = not state.showArchived
		else
			return false
		end
		return true
	end,
	save = function(state)
		return { folds = state.folds, showArchived = state.showArchived }
	end,
})
