# Bot/Assistant Accounts for "On Behalf Of" Authentication

Currently devaipod/service-gator uses Personal Access Tokens (PATs) or API keys
for authenticating to forges and services. This works but has downsides: tokens
often grant broad permissions and users must trust the tool with credentials
that could access all their resources.

## Proposal

Have users create their own OAuth2 application (GitHub App, GitLab Application,
etc.), then authorize devaipod via OAuth. This enables "on behalf of" user
actions similar to how GitHub Copilot works.

## Platform Support

### GitHub: GitHub Apps

GitHub Apps have three authentication modes:

| Mode | Token Prefix | Attribution |
|------|-------------|-------------|
| As the App | JWT | App identity |
| As an Installation | `ghs_*` | App bot (e.g., `my-app[bot]`) |
| **On Behalf of User** | `ghu_*` | **User's identity** |

The "on behalf of user" mode generates user access tokens via OAuth. Actions
appear as the user with a small app badge overlay. Permissions are the
intersection of what the app requests AND what the user has access to.

Key features:
- Device flow supported (for CLI tools)
- User tokens expire after 8 hours, refresh tokens last 6 months
- App Manifest available for easy user setup
- Commits via API default to the authenticated user as author

### GitLab: OAuth2 Applications

GitLab supports standard OAuth2 with all common flows:
- Authorization code (with PKCE)
- Device authorization grant (GitLab 17.2+)
- Resource owner password credentials (discouraged)

Attribution: API calls are attributed to the user who authorized the app. There
is no special "on behalf of" badge like GitHub has, but the audit log shows
the OAuth application that was used.

Key features:
- Device flow supported since GitLab 17.2
- Access tokens expire (default 2 hours), refresh tokens available
- Scopes control access (api, read_user, read_repository, etc.)

### Forgejo/Gitea: OAuth2 Provider

Forgejo supports OAuth2 Authorization Code Grant with PKCE. Similar to GitLab,
there's no special "on behalf of" visual indicator.

Key features:
- PKCE supported for public clients
- OpenID Connect supported
- Git credential helpers work with OAuth tokens
- Note: OAuth2 scopes not yet fully implemented (tokens get full access)

### Google (Docs, Drive, etc.): OAuth2

Google uses standard OAuth2 with comprehensive scope system.

Key features:
- Device flow available for limited-input devices
- Incremental authorization (request scopes as needed)
- Service accounts available for server-to-server (no user context)
- Detailed attribution in audit logs

## Common Pattern

All platforms follow a similar pattern:

1. **User creates an OAuth application** in their account settings
2. **User authorizes the tool** via OAuth flow (device flow for CLI)
3. **Tool receives access token** that acts on user's behalf
4. **Actions are attributed to the user** (with varying levels of app visibility)
5. **Token refresh** handles expiration automatically

## Benefits Over Static Tokens

- **Scoped permissions**: Users define exactly what the app can access
- **Revocable**: Users can revoke app authorization without rotating all tokens
- **User control**: Each user owns their app, not trusting a shared secret
- **Audit trail**: Platforms show actions came via an application
- **Expiring tokens**: Reduced blast radius if tokens are leaked

## Implementation Requirements

### OAuth Device Flow

Since devaipod is CLI-based, use the device flow where supported:
1. User runs setup command
2. Tool displays code and URL
3. User authorizes in browser
4. Tool receives access token

Platforms supporting device flow:
- GitHub: Yes
- GitLab: Yes (17.2+)
- Forgejo: No (must use authorization code with localhost redirect)
- Google: Yes

### Token Lifecycle

| Platform | Access Token TTL | Refresh Token TTL |
|----------|-----------------|-------------------|
| GitHub | 8 hours | 6 months |
| GitLab | 2 hours | configurable |
| Forgejo | configurable | configurable |
| Google | 1 hour | until revoked* |

*Google refresh tokens can expire for various reasons (inactivity, password
change, user revocation, admin policy).

### Minimum Permissions

For typical devaipod usage (working on PRs, pushing branches):

**GitHub App permissions:**
- `contents`: write
- `pull_requests`: write
- `issues`: read
- `metadata`: read

**GitLab scopes:**
- `api` or `read_api` + `write_repository`

**Forgejo:**
- Currently no granular scopes (full access)

## Integration with `devaipod init`

The `devaipod init` command should guide users through OAuth app setup as the
recommended authentication method. Rough UX flow:

```
$ devaipod init

Welcome to devaipod! Let's set up authentication for your forges.

Which forge do you primarily use?
  1. GitHub
  2. GitLab
  3. Forgejo/Gitea
  4. Skip for now

> 1

GitHub Authentication
=====================
We recommend creating a personal GitHub App for secure, scoped access.
This gives you control over permissions and easy revocation.

Would you like to:
  1. Create a new GitHub App (recommended)
  2. Use an existing GitHub App
  3. Use a Personal Access Token (legacy)

> 1

Opening browser to create your GitHub App...
  https://github.com/settings/apps/new?manifest=<encoded-manifest>

After creating the app, paste the App ID: 123456
Paste the Client ID: Iv1.abc123...

Now let's authorize devaipod to act on your behalf.
Opening browser for authorization...
  https://github.com/login/device

Enter the code shown in your browser: ABCD-1234

✓ Authorization successful!
  Token stored in: ~/.config/devaipod/credentials.json
  Refresh token expires: 2026-08-01

Your GitHub App "devaipod-yourname" is configured with:
  - contents: write
  - pull_requests: write  
  - issues: read

To manage or revoke: https://github.com/settings/apps/devaipod-yourname
```

### Key UX Principles

1. **Guided setup**: Walk users through app creation, don't just link to docs
2. **App Manifest**: Use GitHub's manifest flow to pre-fill correct permissions
3. **Device flow**: No need for users to copy callback URLs or run local servers
4. **Transparency**: Show what permissions are granted and how to revoke
5. **Fallback**: Still support PATs for users who prefer them or need them

### App Manifest

For GitHub, we can provide a manifest URL that pre-configures the app:

```json
{
  "name": "devaipod-<username>",
  "url": "https://github.com/cgwalters/devaipod",
  "description": "AI coding agent sandbox",
  "public": false,
  "default_permissions": {
    "contents": "write",
    "pull_requests": "write",
    "issues": "read",
    "metadata": "read"
  },
  "callback_urls": ["http://127.0.0.1/callback"],
  "request_oauth_on_install": true,
  "setup_on_update": true
}
```

The manifest flow:
1. User clicks URL with encoded manifest
2. GitHub shows "Create GitHub App" page with fields pre-filled
3. User clicks "Create GitHub App"
4. GitHub creates the app and redirects with credentials
5. devaipod captures the credentials and proceeds to OAuth

### Credential Storage

Store OAuth credentials securely:

```
~/.config/devaipod/credentials.json  (mode 0600)
{
  "github.com": {
    "type": "github-app",
    "app_id": "123456",
    "client_id": "Iv1.abc123",
    "access_token": "ghu_...",      // encrypted or via keyring
    "refresh_token": "ghr_...",
    "expires_at": "2026-02-01T12:00:00Z"
  }
}
```

Or integrate with system keyring (libsecret on Linux, Keychain on macOS).

For podman integration, could also store as podman secrets and reference them
in `devaipod.toml`.

## service-gator Changes

The `gh` CLI accepts OAuth tokens via `GH_TOKEN`. Similarly, `glab` accepts
GitLab OAuth tokens. Minimal changes needed:

- Handle token refresh before expiry
- Verify token type on startup
- Add `--oauth` mode vs `--pat` mode
- Store refresh tokens securely

## Open Questions

- Should we support a shared "devaipod" app for simpler onboarding?
- How to handle self-hosted instances (different OAuth endpoints)?
- Token storage: extend existing podman secrets or separate credential store?
- How to handle Forgejo's lack of OAuth scopes (full access concern)?

## References

### GitHub
- [About authentication with a GitHub App](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/about-authentication-with-a-github-app)
- [Generating user access tokens](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app)
- [Authenticating on behalf of a user](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/authenticating-with-a-github-app-on-behalf-of-a-user)
- [App Manifests](https://docs.github.com/en/apps/sharing-github-apps/registering-a-github-app-from-a-manifest)

### GitLab
- [OAuth 2.0 identity provider API](https://docs.gitlab.com/ee/api/oauth2.html)
- [Device Authorization Grant](https://docs.gitlab.com/ee/api/oauth2.html#device-authorization-grant-flow)

### Forgejo
- [OAuth2 provider](https://forgejo.org/docs/latest/user/oauth2-provider/)

### Google
- [Using OAuth 2.0 to Access Google APIs](https://developers.google.com/identity/protocols/oauth2)
- [OAuth 2.0 for TV and Limited-Input Device Applications](https://developers.google.com/identity/protocols/oauth2/limited-input-device)
