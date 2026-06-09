# DevTunnel GUI — English (en-US)

## Status bar
status-loading = loading…
status-refreshing = refreshing…
status-port-count = { $count ->
    [one]   1 port
   *[other] { $count } ports
}
status-error = error: { $message }

## Top bar
btn-refresh = Refresh
btn-new-group = New group
btn-add-port = Add port
btn-settings = Settings

## Row actions
btn-del-port = Delete port
btn-del-group = Delete group

## Dialogs — common
btn-cancel = Cancel
btn-create = Create
btn-add = Add
btn-delete = Delete
dlg-advanced = Advanced
dlg-keep-headers = Keep original Host/Origin headers
dlg-request-timeout = Request timeout (seconds)
ph-request-timeout = 0 = disabled
unit-days = days

## Dialog — new group
dlg-new-group-title = New group
field-name = Name
field-expiration = Expiration
field-anonymous = Allow anonymous access
field-description = Description
ph-group-name = e.g. frontend
ph-expiration = e.g. 30d
ph-description = optional

## Dialog — add port
dlg-add-port-title = Add port
field-group = Group
field-port = Port number
field-protocol = Protocol
new-group-option = + New group…
ph-port = e.g. 3000

## Dialog — inline validation
err-field-name-required = Name is required.
err-field-port-required = Port number is required.
err-field-port-range = Port must be between 1 and 65535.

## Confirmations
confirm-delete-group = Delete group "{ $name }" and all of its ports? This cannot be undone.
confirm-delete-port = Delete port { $port } from "{ $group }"? Other ports are not affected.

## Operation status
status-creating-group = creating group…
status-adding-port = adding port…
status-deleting = deleting…

## Port list
no-url = (no URL — port not configured)
expires-label = expires

## Port row actions
btn-copy = Copy
btn-open = Open

## Hosting
btn-host = Host
btn-stop = Stop
status-hosting = hosting…
status-stopped = stopped

## Health badges
badge-operational = Operational
badge-service-down = Service down
badge-down = Down
badge-provisioning = Provisioning…
badge-hosted-external = Hosted elsewhere

## Settings — requirements checklist
req-title = Requirements
req-cli = Dev Tunnels CLI installed
req-login = Signed in
req-login-as = Signed in as
req-installed = Installed in your programs folder
req-shortcut = Start-menu shortcut created
req-autostart = Starts with Windows
btn-install-cli = Install CLI
req-install-hint = Turning on "Start with Windows" installs the app into your user programs folder, adds a Start-menu shortcut, and enables auto-start.

## Settings
settings-title = Settings
settings-section-general = General
settings-section-status = Status
settings-section-about = About
field-auto-start = Start with Windows
field-probe-interval = Probe interval (seconds)
field-default-expiration = Default expiration
field-log-level = Log level
log-level-error = Error
log-level-warn = Warning
log-level-info = Info
log-level-debug = Debug
btn-close = Close

## Settings — uninstall
btn-uninstall = Uninstall
confirm-uninstall = Uninstall DevTunnel GUI? This removes the Start-menu shortcut, turns off start-with-Windows, deletes your saved settings, and removes the app from your programs folder.

## About
about-title = About
about-app-name = Dev Tunnels GUI
about-version-label = Version
about-tagline = Manage Microsoft Dev Tunnels from your Windows tray.
about-built-on = Built on Microsoft Dev Tunnels — Microsoft's free, security-focused tunneling service — and its official CLI and SDK. Not affiliated with or endorsed by Microsoft.
about-created-by = Created by Paulo Corcino
about-link-docs = Microsoft Dev Tunnels docs
about-link-repo = Project on GitHub
about-link-license = MIT License

## Preflight banner / re-login
banner-cli-missing-title = Dev Tunnels CLI not found
banner-cli-missing-body = The `devtunnel` CLI is required but was not found on PATH.
banner-cli-missing-install = Install it with: winget install Microsoft.devtunnel — or set DEVTUNNEL_BIN to the executable path.
banner-relogin-title = Sign in required
banner-relogin-body = Your Dev Tunnels login is missing or has expired. Sign in to continue.
relogin-message = Sign-in expired — sign in again to keep hosting
btn-sign-in = Sign in
banner-action-open-settings = Open Settings

## Install CLI progress / outcome
install-status-running = Installing…
install-status-done = Dev Tunnels CLI installed
install-status-failed = Install failed: { $message }
install-status-elevation = Install needs administrator rights — opening the manual install page
install-status-winget-missing = winget not available — opening the manual install page
status-signing-in = signing in…
toast-relogin-title = DevTunnel GUI — sign-in required
toast-relogin-body = Your Dev Tunnels sign-in expired. Open the app and click "Sign in" to re-authenticate.

## Port detail panel
tab-metrics = Metrics
tab-logs = Logs
metric-upload = Upload
metric-download = Download
metric-total = Total
metric-rate = Rate
metric-connections = Connections
metric-active = Active
metric-na = n/a
metric-rate-per-second = { $value }/s
logs-empty = No log entries yet.

## Status-dot tooltip labels (idle / hosting states; health states reuse badge-*)
badge-stopped = Stopped
badge-hosting = Hosting…

## Top bar (redesign)
app-title = Dev Tunnels
pill-connected = Connected
tooltip-settings = Toggle dark mode

## Row action tooltips
tooltip-copy = Copy URL
tooltip-open = Open in browser

## Toast
toast-copied = URL copied

## Empty state
empty-title = No groups yet
btn-create-group = + Create group

## Tray menu
menu-open-window = Open window
menu-copy-url = Copy URL
menu-open-browser = Open in browser
menu-quit = Quit

## Errors / assertions
err-tray-icon = invalid tray icon
err-cli-not-found = failed to run `devtunnel { $args }` — is the CLI on PATH? (set DEVTUNNEL_BIN if needed)
err-cli-failed = `devtunnel { $args }` returned error: { $stderr }
err-cli-invalid-json = invalid JSON from `devtunnel { $args }`
err-empty-group-name = group name must contain at least one letter or digit
err-invalid-port = port number must be between 1 and 65535
err-port-not-found = port { $port } not found in tunnel { $tunnel }
err-login-failed = sign-in did not complete — close the login window and try again
