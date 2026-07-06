# Microsoft Store listing — TunnelDeck for Dev Tunnels

Copy-paste source for the Partner Center **Store listing** page. All text is in
English (project rule). Character limits are Microsoft's current maximums.

---

## App name (reserved in Partner Center)

```
TunnelDeck for Dev Tunnels
```

> Uses the `<YourApp> for Dev Tunnels` pattern so the Store review accepts it: it
> names your independent product first and references the Microsoft service it
> builds on, without implying Microsoft authorship. Keep the in-app "About"
> disclaimer ("Not affiliated with or endorsed by Microsoft").

## Short description / subtitle (≤ 100 chars)

```
Turn localhost into a secure public HTTPS URL in one click — right from your Windows tray.
```

## Description (≤ 10,000 chars)

```
Share what you're building — instantly.

TunnelDeck puts a public, secure HTTPS URL in front of any service running on
your machine, in a single click. Start your local app, pick the port, and hand
a working link to a teammate, a client, or a webhook — no firewall rules, no
router setup, no config files.

It lives quietly in your Windows tray and stays out of your way until you need
it. When you do, it's a click: create a tunnel, copy the link, and go.

WHY YOU'LL LIKE IT

• One click to public — expose a local port as a live HTTPS URL and copy it to
  your clipboard, ready to paste anywhere.
• Built for demos and testing — show work-in-progress to anyone, anywhere,
  without deploying first.
• Test webhooks the easy way — give Stripe, GitHub, Twilio, or any provider a
  reachable endpoint that points straight at your dev machine.
• Cross-device previews — open your site on a phone, tablet, or a colleague's
  laptop from the same secure link.
• Keep it alive — TunnelDeck keeps your tunnel running and reconnects for you,
  so the link keeps working while you work.
• Private by default — tunnels are authenticated unless you choose to make them
  public, so only the people you want can reach your machine.
• Stays tidy — a clean tray app with a focused window. No dashboards to learn,
  no clutter.

HOW IT WORKS

TunnelDeck is a friendly desktop front end for Microsoft Dev Tunnels — the same
free, security-focused tunneling service used across Visual Studio and VS Code.
Your traffic runs over Microsoft's infrastructure; TunnelDeck just makes it
effortless to create, name, share, and keep tunnels alive from Windows.

You sign in with your own Microsoft, Entra ID, or GitHub account — the identity
Dev Tunnels already uses — and your tunnels are yours.

GOOD TO KNOW

• Requires the free Microsoft Dev Tunnels CLI (devtunnel). If it isn't already
  on your machine, TunnelDeck points you to the one-line install.
• No inbound ports are opened on your machine. Traffic flows outbound over
  HTTPS only.
• Windows tray app. The Dev Tunnels service itself is free.

TunnelDeck is an independent client built on top of the official Microsoft Dev
Tunnels service. It is not affiliated with, sponsored by, or endorsed by
Microsoft.
```

## Product features (Partner Center "Features", ≤ 20 items, ≤ 200 chars each)

```
One click from localhost to a secure public HTTPS URL
Copy-ready links for demos, client previews, and cross-device testing
Point webhooks (Stripe, GitHub, Twilio, …) straight at your dev machine
Keeps tunnels alive and reconnects automatically
Authenticated by default — you decide what's public
Lightweight Windows tray app, no dashboard to learn
Sign in with your own Microsoft, Entra ID, or GitHub account
Outbound HTTPS only — no inbound ports opened
```

## Search terms (Partner Center, ≤ 7 terms, ≤ 30 chars each — not shown to users)

```
tunnel
localhost
dev tunnel
public url
webhook testing
share localhost
reverse proxy
```

## Category

```
Developer tools
```

(Sub-category: Development kits, or Utilities & tools.)

## Copyright / additional info

- **Copyright:** `© 2026 Paulo Corcino`
- **Website:** your GitHub repo or project page (e.g. https://github.com/paulocorcino/devtunnel_gui)
- **Support contact:** paulo@corcino.com.br
- **Privacy policy URL:** required — publish `docs/store/privacy-policy.md` (see below) and paste its public URL.

## What's new in this version (release notes)

```
First Microsoft Store release of TunnelDeck for Dev Tunnels. Create, share, and
keep Microsoft Dev Tunnels alive from your Windows tray.
```

---

## Screenshots (required: at least 1; recommended 3–5)

Store requirements for desktop: PNG, **1366 × 768** or larger, 16:9 preferred.
Capture from the running app (light and/or dark theme):

1. Main window with a couple of tunnels, one showing a live public URL.
2. Creating a tunnel / adding a port.
3. The tray icon + menu.
4. Settings (General) — probe interval, default expiration, log level.
5. About panel (shows the Microsoft attribution + disclaimer).

Tip: capture at 1920 × 1080 for crisp thumbnails. Store store-side scaling handles
the rest. Add a one-line caption per screenshot in Partner Center.

## Age rating (IARC questionnaire)

TunnelDeck is a developer utility with no in-app content, ads, purchases, or
user-generated content that the app itself hosts. Expected answers:

- Contains violence / sexual / profanity / controlled substances: **No** to all.
- Users can interact / share content / exchange location or personal info: **No**
  (the app creates network tunnels for the user's own services; it is not a
  social or communication platform).
- Collects/shares personal data for advertising: **No**.

Expected outcome: **Everyone / PEGI 3 / ESRB Everyone**. Answer the questionnaire
truthfully in Partner Center; IARC assigns the rating automatically.

## Store submission checklist

- [ ] App name reserved (`TunnelDeck for Dev Tunnels`).
- [ ] Package identity values copied into the manifest via build-msix.ps1.
- [ ] **Unsigned** .msix uploaded (Store re-signs; a signed package is rejected).
- [ ] WACK passed locally.
- [ ] Description, features, search terms, category filled in.
- [ ] ≥ 1 screenshot (1366×768+).
- [ ] Privacy policy URL live and reachable.
- [ ] Age rating questionnaire completed.
- [ ] Support email + copyright set.
