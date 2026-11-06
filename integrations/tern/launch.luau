local tern = require("tern")
local M = {}
local opening = {}

function M.absolute(path)
	return type(path) == "string" and (path:sub(1, 1) == "/" or path:match("^%a:[/\\]") ~= nil or path:sub(1, 2) == "\\\\")
end

local function argument(value, label)
	assert(type(value) == "string" and value ~= "" and not value:find("[%z\r\n]"), "Invalid " .. label)
	return value
end

function M.config()
	local path = tern.plugin.data .. "/config.json"
	local value = tern.json.decode(tern.fs.read(path, 65536))
	assert(type(value) == "table", "Expected an object in " .. path)
	argument(value.bridge, "bridge path")
	argument(value.config, "helper config path")
	assert(M.absolute(value.bridge), "bridge must be an absolute path in " .. path)
	assert(M.absolute(value.config), "config must be an absolute path in " .. path)
	return {
		bridge = value.bridge,
		config = value.config,
		tern = argument(value.tern or tern.getenv("TERN_EXE") or "tern", "Tern executable"),
	}
end

local function request(config, args, done)
	local argv = { config.tern }
	for _, arg in args do
		table.insert(argv, arg)
	end
	local ok, err = pcall(tern.process.run, argv, { timeout_ms = 10000 }, function(result)
		if result.status ~= 0 then
			done(nil, result.stderr ~= "" and result.stderr or "Tern 命令未完成")
			return
		end
		local decoded, value = pcall(tern.json.decode, result.stdout)
		if not decoded or type(value) ~= "table" then
			done(nil, "Tern 返回了无效的操作结果")
			return
		end
		done(value, nil)
	end)
	if not ok then
		done(nil, tostring(err))
	end
end

local function contains(info, sourcePane)
	for _, pane in info.panes or {} do
		if pane.pane == sourcePane then
			return true
		end
	end
	return false
end

local function owner(config, sourcePane, done)
	request(config, { "inspect", "--window", "", "--json" }, function(root, err)
		if err then done(nil, err); return end
		if contains(root, sourcePane) then done("", nil); return end
		local windows, seen = {}, { [""] = true }
		for _, client in root.clients or {} do
			if client.kind ~= "cli" and type(client.window) == "string" and not seen[client.window] then
				seen[client.window] = true
				table.insert(windows, client.window)
			end
		end
		local function nextWindow(index)
			local window = windows[index]
			if window == nil then
				done(nil, "项目树所在的客户端已断开，请重新打开项目树")
				return
			end
			request(config, { "inspect", "--window", window, "--json" }, function(info, inspectError)
				if not inspectError and contains(info, sourcePane) then
					done(window, nil)
				else
					nextWindow(index + 1)
				end
			end)
		end
		nextWindow(1)
	end)
end

-- CLI launches are shell commands in `inspect`, not the helper's original argv.
-- Decode quoting only for identity comparison; never evaluate the command.
local function commandArgs(command)
	local args, word, quote, escaped, started = {}, "", nil, false, false
	for index = 1, #command do
		local char = command:sub(index, index)
		if escaped then
			word ..= char
			escaped = false
		elseif char == "\\" and quote ~= "'" then
			escaped = true
			started = true
		elseif quote then
			if char == quote then quote = nil else word ..= char end
		elseif char == "'" or char == '"' then
			quote = char
			started = true
		elseif char:match("%s") then
			if started then table.insert(args, word); word = ""; started = false end
		else
			word ..= char
			started = true
		end
	end
	if quote or escaped then return {} end
	if started then table.insert(args, word) end
	return args
end

local function attached(info, config, session)
	for _, entry in info.sessions or {} do
		for _, tab in entry.tabs or {} do
			for _, block in tab.blocks or {} do
				local args = if type(block.command) == "string" then commandArgs(block.command)
					else { block.program, table.unpack(block.args or {}) }
				if #args == 5 and args[1] == config.bridge
					and args[2] == "--config" and args[3] == config.config
					and args[4] == "attach" and args[5] == session
					and (block.exited == nil or block.exited == tern.json.null) then
					return block.id, tab.id
				end
			end
		end
	end
	return nil
end

function M.open(config, sourcePane, session, done)
	-- Route by the source pane's actual client. The installed CLI ignores TERN_PANE
	-- for this operation; its default window could otherwise open the terminal on a desktop.
	owner(config, sourcePane, function(window, err)
		if err then done(err); return end
		local key = window .. "\0" .. config.bridge .. "\0" .. config.config .. "\0" .. session
		if opening[key] then done(nil); return end
		opening[key] = true
		local function finish(message)
			opening[key] = nil
			done(message)
		end
		local unnamed, sourceTab = false, nil
		local function focus(block)
			local function reveal()
				request(config, { "focus", tostring(block), "--window", window, "--json" }, function(_, focusError)
					finish(focusError)
				end)
			end
			if unnamed then
				-- A default tab title follows the focused terminal; keep the project entry
				-- stable while preserving any name the user explicitly assigned.
				request(config, { "rename", tostring(sourcePane), "Falcon Projects", "--window", window, "--json" }, function(_, nameError)
					if nameError then finish(nameError); return end
					reveal()
				end)
			else
				reveal()
			end
		end
		-- Inspect after taking the shared guard: separate project trees can click the
		-- same session while an earlier owner lookup is still in flight.
		request(config, { "inspect", "--window", window, "--json" }, function(info, inspectError)
			if inspectError then finish(inspectError); return end
			for _, entry in info.sessions or {} do
				for _, tab in entry.tabs or {} do
					for _, block in tab.blocks or {} do
						if block.id == sourcePane then
							sourceTab = tab.id
							unnamed = type(tab.name) ~= "string" or tab.name == ""
						end
					end
				end
			end
			local existing, existingTab = attached(info, config, session)
			if existing then
				if sourceTab and existingTab ~= sourceTab then
					request(config, { "move", tostring(existing), "right-of", tostring(sourcePane), "--window", window, "--json" }, function(_, moveError)
						if moveError then finish(moveError); return end
						focus(existing)
					end)
				else
					focus(existing)
				end
				return
			end
			-- A split belongs to the project tree's tab, so no extra sidebar tab is added.
			request(config, { "split", tostring(sourcePane), "right", "--window", window, "--json", "--",
				config.bridge, "--config", config.config, "attach", session,
			}, function(opened, openError)
				if openError then finish(openError); return end
				if type(opened.block) ~= "number" then
					finish("Tern 返回了无效的终端标识")
					return
				end
				focus(opened.block)
			end)
		end)
	end)
end

return M
