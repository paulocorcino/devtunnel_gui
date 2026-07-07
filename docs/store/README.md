# Publishing to the Microsoft Store

End-to-end runbook for shipping **TunnelDeck for Dev Tunnels** to the Microsoft
Store as an MSIX package. Work top to bottom; each step links to the artifact that
implements it.

| Artifact | Purpose |
|---|---|
| `store` cargo feature | Compiles out self-install, the GitHub update checker, and the HKCU auto-start (all MSIX-incompatible / against policy). Auto-start moves to the manifest. |
| `packaging/msix/AppxManifest.xml` | Package manifest: identity placeholders, full-trust app, `windows.startupTask`. |
| `packaging/msix/build-msix.ps1` | Builds the exe, renders assets, packs the `.msix`, optional sign + WACK. |
| `src/bin/gen_msix_assets.rs` | Renders the tile/logo PNGs from the app icon (run by the script). |
| `docs/store/listing.md` | Store listing copy: name, description, features, keywords, screenshots, age rating. |
| `docs/store/privacy-policy.md` | Privacy policy to publish and link (required). |

---

## Step 1 — Partner Center account & app name

1. Create a **Microsoft Partner Center** developer account (one-time fee: ~US$19
   individual / US$99 company): https://partner.microsoft.com/dashboard/registration
2. **Apps and games → New product → MSIX or PWA app.**
3. **Reserve the name** `TunnelDeck for Dev Tunnels`.
   - The `<YourApp> for Dev Tunnels` form is used deliberately: it avoids a
     trademark rejection for leading with Microsoft's product name. Do **not**
     reserve just "Dev Tunnels …".
4. Open **Product → Product identity** and copy these three values — you'll pass
   them to `build-msix.ps1`:
   - **Package/Identity/Name** → `-IdentityName`
   - **Package/Identity/Publisher** (`CN=…`) → `-PublisherId`
   - **Publisher display name** → `-PublisherDisplayName`

## Step 2 — Build the store executable

The `store` feature strips the MSIX-incompatible bits and **pulls in `hosting`**
(the Host button is core to the product). That builds the `tunnels` SDK + vendored
OpenSSL, which needs **NASM** and **Strawberry Perl** on `PATH` (see the repo
`CLAUDE.md`). On this machine, prepend before building:

```powershell
$env:PATH = "C:\Strawberry\perl\bin;C:\Strawberry\c\bin;C:\Users\PICHAU\AppData\Local\bin\NASM;$env:PATH"
cargo build --release --features store --bin devtunnel_gui
```

`build-msix.ps1` runs this for you.

## Step 3 — Fill in the manifest identity & package

Put the three Partner Center identity values into a `.env` file (gitignored), then
run the script with no arguments. `build-msix.ps1` builds the exe, renders
`Assets\`, substitutes the identity into the manifest, and packs the `.msix`.

```powershell
cd packaging\msix
Copy-Item .env.example .env
notepad .env      # fill IDENTITY_NAME, PUBLISHER_ID, PUBLISHER_DISPLAY_NAME
.\build-msix.ps1
```

(You can still override any value on the command line, e.g. `-Version 0.2.0.0`.)

Output: `packaging\msix\out\TunnelDeck-0.1.0.0.msix` (**unsigned** — correct for
submission; the Store re-signs it).

## Step 4 — Test locally + certify (WACK)

The submission package is unsigned, but to **install and test locally** you need a
self-signed cert whose subject exactly equals `Identity/@Publisher`:

```powershell
# One-time: create a test cert (subject must match your -PublisherId)
$cert = New-SelfSignedCertificate -Type Custom -Subject "CN=Paulo Corcino" `
  -KeyUsage DigitalSignature -CertStoreLocation "Cert:\CurrentUser\My" `
  -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
Export-PfxCertificate -Cert $cert -FilePath .\TunnelDeck-test.pfx `
  -Password (ConvertTo-SecureString -String "test" -Force -AsPlainText)

# Build a signed test package (identity comes from .env):
.\build-msix.ps1 -Sign

# Install it (self-signed → first trust the cert; needs an ELEVATED prompt):
Import-PfxCertificate -FilePath .\TunnelDeck-test.pfx `
  -CertStoreLocation Cert:\LocalMachine\TrustedPeople `
  -Password (ConvertTo-SecureString "test" -Force -AsPlainText)
Add-AppxPackage .\out\TunnelDeck-0.1.0.0.msix

# Run the certification kit — WACK requires an ELEVATED (Administrator) prompt:
.\build-msix.ps1 -Sign -Wack
```

Fix any **WACK** failures before submitting. Then rebuild **without** `-Sign` to
produce the clean unsigned package for upload.

Smoke-test the installed app:
- Launches to the tray; window opens; tunnels list loads.
- Settings → General shows **no** "Start with Windows" toggle (managed by the
  package). Settings → Status shows **no** install/uninstall rows.
- No "update available" banner appears (checker compiled out).
- Enable auto-start via **Windows Settings → Apps → Startup** and confirm it
  launches at logon.

## Step 5 — Create the submission

In Partner Center, on the reserved product:

1. **Packages** — upload the unsigned `.msix`. Set device family to **Desktop**.
2. **Store listing** — paste everything from [`listing.md`](listing.md):
   name, short + full description, features, search terms, category
   (Developer tools), copyright, support email, and screenshots (≥ 1, 1366×768+).
3. **Privacy policy URL** — `https://paulocorcino.github.io/devtunnel_gui/`.
   The `Deploy Pages` workflow publishes [`site/index.html`](site/index.html)
   (mirror of [`privacy-policy.md`](privacy-policy.md)). One-time: repo
   **Settings → Pages → Source = GitHub Actions**. Required field.
4. **Age ratings** — complete the IARC questionnaire (see `listing.md`; expected
   result: Everyone / PEGI 3).
5. **Pricing and availability** — Free; pick markets.
6. **Submit for certification.** Microsoft's automated + manual review typically
   takes hours to a couple of days. If rejected, the report says why — the most
   likely notes here are name/trademark or the CLI dependency; address and
   resubmit.

## Recurring: shipping an update

1. Bump the version (e.g. `-Version 0.2.0.0`; the 4th part must stay `0`).
2. Re-run `build-msix.ps1`, re-test, upload the new unsigned `.msix`.
3. Update **What's new** and submit. The Store delivers the update to users; the
   in-app updater stays disabled in this build by design.
