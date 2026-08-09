local repository = settings.repository or "robsgh/stelle"
local response = http.get("https://api.github.com/repos/" .. repository, {
    Accept = "application/vnd.github+json"
})

if response.status ~= 200 then
    error("GitHub returned HTTP " .. tostring(response.status))
end

local repo = json.decode(response.body)

return {
    title = "Stelle on GitHub",
    subtitle = repo.description or repository,
    href = repo.html_url,
    metrics = {
        { label = "Stars", value = repo.stargazers_count },
        { label = "Forks", value = repo.forks_count },
        { label = "Open issues", value = repo.open_issues_count }
    },
    fetched_at = os.date("!%Y-%m-%dT%H:%M:%SZ")
}
