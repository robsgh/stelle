local base_url = settings.base_url or "https://proxmox.example.com:8006"
local token_id = settings.token_id or ""
local token_secret = settings.token_secret or ""

if token_id == "" or token_secret == "" or token_secret == "REPLACE_WITH_PROXMOX_API_TOKEN_SECRET" then
    error("Configure the Proxmox token_id and token_secret settings")
end

base_url = string.gsub(base_url, "/+$", "")

local response = http.get(base_url .. "/api2/json/cluster/resources", {
    Authorization = "PVEAPIToken=" .. token_id .. "=" .. token_secret
})

if response.status == 401 then
    error("Proxmox rejected the API token")
end

if response.status ~= 200 then
    error("Proxmox returned HTTP " .. tostring(response.status))
end

local payload = json.decode(response.body)
local resources = payload.data or {}

local nodes = 0
local online_nodes = 0
local guests = 0
local running_guests = 0
local cpu_used = 0
local cpu_total = 0
local memory_used = 0
local memory_total = 0
local storage_used = 0
local storage_total = 0

for _, resource in ipairs(resources) do
    local kind = resource.type

    if kind == "node" then
        nodes = nodes + 1
        if resource.status == "online" then
            online_nodes = online_nodes + 1
            local cores = tonumber(resource.maxcpu) or 0
            cpu_used = cpu_used + ((tonumber(resource.cpu) or 0) * cores)
            cpu_total = cpu_total + cores
            memory_used = memory_used + (tonumber(resource.mem) or 0)
            memory_total = memory_total + (tonumber(resource.maxmem) or 0)
        end
    elseif kind == "qemu" or kind == "lxc" then
        guests = guests + 1
        if resource.status == "running" then
            running_guests = running_guests + 1
        end
    elseif kind == "storage" and resource.status == "available" then
        storage_used = storage_used + (tonumber(resource.disk) or 0)
        storage_total = storage_total + (tonumber(resource.maxdisk) or 0)
    end
end

local function percentage(used, total)
    if total <= 0 then
        return "—"
    end
    return tostring(math.floor((used / total * 100) + 0.5)) .. "%"
end

local subtitle
if nodes == 0 then
    subtitle = "No nodes reported"
elseif online_nodes == nodes then
    subtitle = "All nodes online"
else
    subtitle = tostring(nodes - online_nodes) .. " node(s) offline"
end

return {
    title = settings.title or "Proxmox",
    subtitle = subtitle,
    href = base_url,
    metrics = {
        { label = "Nodes", value = tostring(online_nodes) .. "/" .. tostring(nodes) },
        { label = "Guests", value = tostring(running_guests) .. "/" .. tostring(guests) },
        { label = "Stopped", value = guests - running_guests },
        { label = "CPU", value = percentage(cpu_used, cpu_total) },
        { label = "Memory", value = percentage(memory_used, memory_total) },
        { label = "Storage", value = percentage(storage_used, storage_total) }
    },
    fetched_at = os.date("!%Y-%m-%dT%H:%M:%SZ")
}
