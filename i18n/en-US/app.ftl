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

## Port list
no-url = (no URL — port not configured)
expires-label = expires

## Port row actions
btn-copy = Copy
btn-open = Open

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
