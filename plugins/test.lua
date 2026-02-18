---@diagnostic disable: lowercase-global

plugin = {
    name = "test",
    version = "1.0",
    author = "spixa",
    description = "Test plugin"
}

local bad_words = {
    "cranker",
    "cranka",
    "wireback",
    "tinskin"
}

-- required to be a valid plugin
function on_join(ctx) end
function on_leave(ctx) end

local swear_count = {}
local max_swears = 5
function on_message(ctx)
    local message = ctx:get_message()
    local user = ctx:get_username()
    local lower_message = message:lower()

    for _, word in ipairs(bad_words) do
        local pattern = "%f[%a]" .. word .. "%f[%A]" -- this prevents things like grass being filtered if 'ass' is a swearword or so i was told
        if lower_message:find(pattern) then
            swear_count[user] = (swear_count[user] or 0) + 1

            ctx:reply("Your message contains inappropriate language and was blocked. (" .. swear_count[user] .. "/" .. max_swears .. ")")
            ctx:cancel()

            if swear_count[user] >= max_swears then
                ctx:kick("Your message contained bad language! If this is not intended by the administrator tell them to remove the `test.lua` plugin")
            end

            return  -- shortcircuit
        end
    end
end


