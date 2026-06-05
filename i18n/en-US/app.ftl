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

## Status-dot tooltip labels (idle / hosting states; health states reuse badge-*)
badge-stopped = Stopped
badge-hosting = Hosting…

## Top bar (redesign)
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
