---@diagnostic disable: lowercase-global
plugin = {
    name="Welcomer",
    version="0.1",
    author="spixa",
    description="Show welcome message when remote joins server"
}

-- configurations
local server_name = "Another VoUDP Server"
local welcome_msg = "Welcome to %s!\nServer time is %s"

function on_join(ctx) 
    if ctx:get_channel_id() ~= "1" then
        ctx:cancel() -- only allow joining general
    end

    ctx:reply(string.format(welcome_msg, server_name, Core.system_time()))

    if Core.starts_with(ctx:get_addr(), Core.LOOPBACK) then
        ctx:reply("Connected from loopback")
    end
end