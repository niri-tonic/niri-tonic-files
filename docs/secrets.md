# Credential sources and chezmoi

Each password, optional Filen 2FA code, and Google OAuth client secret accepts
exactly **one** of these forms:

```yaml
password: { value: "YOUR_PASSWORD" }
# Or:
password: { env: FILEN_PERSONAL_PASSWORD }
# Or:
password: { secret_service: filen-personal-password }
```

`value` is an explicit literal string. It is used as-is, including whitespace,
quotes, backslashes and newlines; the explorer performs no shell expansion or
template evaluation. Empty strings, NUL characters, non-string values and combined
sources are rejected. A bare scalar such as `password: text` is still rejected.

The same syntax applies to `oauth.client_secret` and `auth.two_factor_code`.
With `value`, no environment variable or keyring is required at application launch.
Backend credentials still travel over the private stdin pipe, not command arguments
or the adapter environment. Debug formatting and validation errors redact values.
Saving configuration preserves the explicitly supplied literal.

## Generate the file with chezmoi and gopass

Use a private template in your chezmoi source state, for example:

```text
dot_config/niri-tonic-files/private_config.yaml.tmpl
```

Merge these instances into your existing configuration template:

```gotemplate
connectors:
  instances:
    - id: filen-personal
      name: Personal Filen
      type: filen
      enabled: true
      auth:
        email: personal@example.com
        password:
          value: {{ gopass "cloud/filen/password" | trim | toJson }}

    - id: drive-personal
      name: Personal Google Drive
      type: google_drive
      enabled: true
      oauth:
        client_id: YOUR_DESKTOP_CLIENT_ID.apps.googleusercontent.com
        client_secret:
          value: {{ gopass "cloud/google-drive/client-secret" | trim | toJson }}
        persist_session: true
```

Replace the gopass paths with your own entry names. `gopass` returns the first
line of an entry. `toJson` produces a quoted, escaped string that is valid YAML:
do not add extra quotes around the template expression. Omit `trim` if leading or
trailing spaces are intentional parts of the secret.

The template stores the lookup expressions; the rendered target file contains
the plaintext secrets. The `private_` attribute makes that target owner-only
(`0600`). Rendered diffs also contain the values, so inspect them locally.

Preview and apply only the application file:

```sh
chezmoi status ~/.config/niri-tonic-files/config.yaml
chezmoi diff ~/.config/niri-tonic-files/config.yaml
chezmoi apply ~/.config/niri-tonic-files/config.yaml
./target/release/niri-tonic-files --check-config
```

Restart the explorer after applying changes. A literal 2FA code still expires;
updating a template does not make an old code reusable.
