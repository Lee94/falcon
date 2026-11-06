local tern = require("tern")

tern.command({
	id = "projects",
	title = "Open Falcon projects",
	icon = "terminal",
	run = function(cx)
		if not cx:new_block("falcon.projects", {}, "tab") then
			cx:toast("error", "Falcon is unavailable on this Tern host", "Install the Falcon plugin and helper on the host serving these panes.")
		end
	end,
})

