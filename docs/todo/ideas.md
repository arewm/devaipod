# Improved service-gator control

devaipod service-gator add <pod> https://github.com/org/repo

And of course any fine-grained scopes.

# Improved handling of git state

Can we detect when the git tree has commits that aren't pushed,
and make that clearer?

# Login as bot

While we have a workspace pod that has full privileges (e.g. GH_TOKEN)
so that a user can do their own pushes or other arbitrary code,
a major downside I've encountered is that the agent pod doesn't share
container images with the workspace pod.

Related to this...in many cases one might want to actually manually
do things in the agent context. I think `devaipod attach` should
probably have *three* TUIs: one with the opencode UX and one that
is basically "workspace terminal" and "agent terminal".

A few things first let's set distinct clear env vars like
`CONTAINER_NAME=agent` `CONTAINER_NAME=workspace` etc. so bash
prompts can distinguish.

(Actually bigger picture on this topic we will likely need to get
 away from tmux into a custom app)

# Improved "done" vs "active" state

For `run` the pod should probably stop by default when the agent
reaches idle state. Make that clear.

But we also need to have improved checking for if the agent reached
a valid "success" state for the task. Do we need to offer a MCP
tool it needs to call? Or maybe if it reaches idle we ping it one
more time and ask it to say complete or not.

# Really awesome review process

**Status: Active design. See:**
- [lightweight-review.md](./lightweight-review.md) — lightweight options starting with extending the OpenCode web UI (recommended starting point)
- [forgejo-integration.md](./forgejo-integration.md) — full local Forgejo spec (longer-term / when CI/CD is needed)

The lightweight approach extends opencode's existing changes UI with commit-range
review, approve/reject controls, and upstream sync via service-gator. The full
Forgejo integration remains the plan for when we need local CI/CD or a multi-repo
dashboard.

# Remote devcontainer integration

Support connecting via Zed/VSCode remote

# Opinionated GUI

Should we have an AutoClaude like frontend?

The current path is extending the opencode web UI via a fork — see
[opencode-webui-fork.md](./opencode-webui-fork.md). This gives us a git
browser, commit-range review, and pod management in a single SPA built on
opencode's existing SolidJS/TS stack. Forgejo remains an option for when
we need local CI/CD.

For TUI and broader UI improvements (session titles, card layout, attach
experience), see [ui.md](./ui.md).

# Kubernetes support

**Status: Research complete, see [kubernetes.md](./kubernetes.md)**

Three deployment models: devaipod in k8s, spawning workspace pods in a cluster,
and hybrid local-devaipod with remote kubeconfig.

# Local Forgejo instance

**Status: Spec complete, see [forgejo-integration.md](./forgejo-integration.md)**

Default-enabled local Forgejo provides:
- Git caching (fast clones from localhost)
- Local CI/CD (Forgejo Actions)
- Code review UI
- PR-based workflow for agent changes

# Local caching: containers

For nested devenv it's super painful to pull container
images into each one. If we fixed https://github.com/containers/container-libs/issues/144
we could optimize w/reflinks which would be amazing.

# Nesting

It would make sense to support say a MCP tool where
a devaipod could request more devaipods in the general case...

# More testing

Our testing story needs to be improved across the board of
course.

# Bot/Assistant Accounts

**Status: Spec complete, see [bot-assistant-accounts.md](./bot-assistant-accounts.md)**

Use OAuth2 apps (GitHub Apps, GitLab Applications, etc.) with "on behalf of"
user authentication instead of PATs. Actions are attributed to the user.
