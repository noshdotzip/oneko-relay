# oneko relay

Minimal websocket relay for multiplayer `oneko-desktop` lobbies.

## GHCR publishing

This repo includes a GitHub Actions workflow at `.github/workflows/relay-image.yml` that publishes a container image to:

- `ghcr.io/<owner>/oneko-relay:latest`
- branch, tag, and commit-SHA tags

The workflow triggers on pushes to `main` or `master`, on version tags like `v0.1.0`, and by manual dispatch.

## Portainer stack

After the repository is pushed to GitHub and the workflow has published the image, use `compose.ghcr.yaml` in Portainer and replace:

- `ghcr.io/OWNER/oneko-relay:latest`

with your actual image path, for example:

- `ghcr.io/nosh/oneko-relay:latest`

If the package is private, configure registry credentials in Portainer for `ghcr.io`.
