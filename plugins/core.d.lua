-- do not delete plugin core

-- plugins/core.d.lua
plugin = { name = "Core", version = "1.0", author = "VoUDP core", description = "Core functionality and LSP stub" }
Core = {}

--- Check if string s starts with prefix
---@param s string
---@param prefix string
---@return boolean
function Core.starts_with(s, prefix) return false end

--- Get system time
---@return string
function Core.system_time() return "" end

--- Print into info channel
---@param message string
function Core.info(message) end

--- Print into warn channel
---@param message string
function Core.warn(message) end

--- Print into error channel
---@param message string
function Core.error(message) end

--- @type string  Loopback ip address
Core.LOOPBACK = ""

--- @type string Protocol version
Core.PROTOCOL_VERSION = ""