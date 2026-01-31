# Improved service-gator control

devaipod service-gator add <pod> https://github.com/org/repo

And of course any fine-grained scopes.

# Improved handling of git state

Can we detect when the git tree has commits that aren't pushed,
and make that clearer?

# Improved "done" vs "active" state

For `run` the pod should probably stop by default when the agent
reaches idle state. Make that clear.

But we also need to have improved checking for if the agent reached
a valid "success" state for the task. Do we need to offer a MCP
tool it needs to call? Or maybe if it reaches idle we ping it one
more time and ask it to say complete or not.

# Remote devcontainer integration

Support connecting via Zed/VSCode remote

# Opinionated GUI

Should we have an AutoClaude like frontend?

# Kubernetes support

We should also support spawning remote pods given
a kubeconfig.

# Local caching: git

We could default run a forgejo instance and have it be
a local git cache, this would likely speed things up
a lot.

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
