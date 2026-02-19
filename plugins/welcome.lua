---@diagnostic disable: lowercase-global
plugin = {
    name="Welcomer",
    version="0.1",
    author="spixa",
    description="Show welcome message when remote joins server"
}

local server_name = "Another VoUDP Server"

function on_join(ctx) 
    if ctx:get_channel_id() ~= "1" then
        ctx:cancel() -- only allow joining general
    end

    ctx:reply(string.format("Welcome to %s!\nTime is %s", server_name, Core.system_time()))

    if Core.starts_with(ctx:get_addr(), Core.LOOPBACK) then
        ctx:reply("Connected from loopback")
    end
end