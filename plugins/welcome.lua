---@diagnostic disable: lowercase-global
plugin = {
    name="Welcomer",
    version="0.1",
    author="spixa",
    description="Show welcome message when remote joins server"
}

function on_join(ctx) 
    ctx:reply("Welcome to the server! You have joined channel " .. ctx:get_channel_id() .. "\nTime is " .. Core.system_time())

    if Core.starts_with(ctx:get_addr(), Core.LOOPBACK) then
        ctx:reply("Connected from loopback")
    end
end