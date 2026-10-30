## Download

- **[tini-setup.zip](https://github.com/dimas3007/tini/releases/latest/download/tini-setup.zip)**: installer for Windows 10/11 (x64). Installs per user to `%LOCALAPPDATA%\Programs\tini`, no admin rights needed.
- **tini-portable.zip**: the same binaries without an installer.

The binaries are not code-signed yet, so Windows SmartScreen may show "Windows protected your PC". Choose **More info → Run anyway**, or check the file first using the hashes and links below.

## Verify

SHA-256 of {{VERSION}}:

```
{{SHA256SUMS}}
```

- VirusTotal: <!-- paste the VirusTotal link for tini-setup.exe before publishing -->
- Build provenance: `gh attestation verify tini-setup.exe --repo dimas3007/tini` (built by GitHub Actions from this tag)

tini is free and open source (MIT OR Apache-2.0), and everything in it stays free.
