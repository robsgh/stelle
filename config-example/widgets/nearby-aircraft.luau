local base_url = settings.base_url or "https://planes.example.com"
local max_age = tonumber(settings.max_age) or 60
local radius_nm = tonumber(settings.radius_nm) or 50
local receiver_lat = tonumber(settings.receiver_lat)
local receiver_lon = tonumber(settings.receiver_lon)

base_url = string.gsub(base_url, "/+$", "")

local response = http.get(base_url .. "/data/aircraft.json")
if response.status ~= 200 then
    error("tar1090 returned HTTP " .. tostring(response.status))
end

local payload = json.decode(response.body)
local selected = nil
local selected_distance = nil
local selected_signal = -1000
local tracked = 0

local function distance_nm(lat, lon)
    if receiver_lat == nil or receiver_lon == nil then
        return nil
    end
    local radians = math.pi / 180
    local receiver_lat_rad = receiver_lat * radians
    local aircraft_lat_rad = lat * radians
    local cosine = math.sin(receiver_lat_rad) * math.sin(aircraft_lat_rad)
        + math.cos(receiver_lat_rad) * math.cos(aircraft_lat_rad)
        * math.cos((lon - receiver_lon) * radians)
    cosine = math.max(-1, math.min(1, cosine))
    return math.acos(cosine) * 3440.065
end

for _, aircraft in ipairs(payload.aircraft or {}) do
    local seen = tonumber(aircraft.seen) or 999
    local lat = tonumber(aircraft.lat)
    local lon = tonumber(aircraft.lon)
    if seen <= max_age and lat ~= nil and lon ~= nil then
        local distance = distance_nm(lat, lon)
        if distance == nil or distance <= radius_nm then
            tracked = tracked + 1
            local signal = tonumber(aircraft.rssi) or -1000
            local closer = distance ~= nil
                and (selected_distance == nil or distance < selected_distance)
            local stronger = distance == nil and signal > selected_signal
            if selected == nil or closer or stronger then
                selected = aircraft
                selected_distance = distance
                selected_signal = signal
            end
        end
    end
end

local function trim(value)
    return string.gsub(value or "", "^%s*(.-)%s*$", "%1")
end

local function rounded(value)
    local number = tonumber(value)
    if number == nil then
        return nil
    end
    return math.floor(number + 0.5)
end

local function altitude(aircraft)
    if aircraft.alt_baro == "ground" then
        return "Ground"
    end
    local value = rounded(aircraft.alt_baro or aircraft.alt_geom)
    return value ~= nil and tostring(value) .. " ft" or "—"
end

local function speed(aircraft)
    local value = rounded(aircraft.gs)
    return value ~= nil and tostring(value) .. " kt" or "—"
end

local function heading(aircraft)
    local value = rounded(aircraft.track or aircraft.nav_heading)
    return value ~= nil and tostring(value % 360) .. "°" or "—"
end

local function vertical_rate(aircraft)
    local value = rounded(aircraft.baro_rate or aircraft.geom_rate)
    if value == nil then
        return "—"
    end
    local prefix = value > 0 and "+" or ""
    return prefix .. tostring(value) .. " fpm"
end

if selected == nil then
    return {
        title = "Nearby Aircraft",
        subtitle = receiver_lat ~= nil and "No aircraft within " .. tostring(radius_nm) .. " nm"
            or "No recent aircraft positions",
        href = base_url,
        metrics = {
            { label = "Tracked", value = 0 }
        },
        fetched_at = os.date("!%Y-%m-%dT%H:%M:%SZ")
    }
end

local flight = trim(selected.flight)
local hex = string.upper(selected.hex or "Unknown")
local title = flight ~= "" and flight or hex
local identity_label = flight ~= "" and "Callsign" or "ICAO address"
local subtitle
local proximity_label
local proximity_value
if selected_distance ~= nil then
    subtitle = identity_label .. " · "
        .. string.format("Closest of %d aircraft within %.0f nm", tracked, radius_nm)
    proximity_label = "Distance"
    proximity_value = string.format("%.1f nm", selected_distance)
else
    subtitle = identity_label .. " · Strongest nearby signal · "
        .. tostring(tracked) .. " aircraft tracked"
    proximity_label = "Signal"
    proximity_value = selected_signal > -1000 and string.format("%.1f dBFS", selected_signal) or "—"
end

return {
    title = title,
    subtitle = subtitle,
    href = base_url .. "/?icao=" .. (selected.hex or ""),
    metrics = {
        { label = proximity_label, value = proximity_value },
        { label = "Altitude", value = altitude(selected) },
        { label = "Speed", value = speed(selected) },
        { label = "Heading", value = heading(selected) },
        { label = "Vertical", value = vertical_rate(selected) }
    },
    fetched_at = os.date("!%Y-%m-%dT%H:%M:%SZ")
}
