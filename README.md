# Forgejo SSP (Static Site Provider)

A lightweight, self-hosted Static Site Provider (SSP) designed to bring a "GitHub Pages" style experience to your self-hosted [Forgejo](https://forgejo.org/) (or Gitea) instance. 

## Objective

Allow users to seamlessly host and serve static websites directly from their self-hosted Git repositories, utilizing automatic subdomain routing and local caching.

## Features

* **Dynamic Routing:** Automatically maps subdomains to specific Forgejo users and repositories.
* **On-the-Fly Cloning:** Fetches the required repository branch only when a request is made.
* **Smart Caching:** Caches static files locally with a 1-hour TTL (Time To Live) to ensure fast load times and reduce strain on the Git server.
* **Private Repo Support:** Authenticate with a Git token to serve private repositories safely.

## How It Works

1. **Routing:** `Forgejo_SSP` is hosted and configured to map the host header of incoming HTTP requests to a specific URL on your Git server.
    * **Example 1 (Default User):** `{repo}.ssp.mydomain.tld` -> routes to `git.mydomain.tld/{default_user}/{repo}`
    * **Example 2 (Specific User):** `{repo}.{user}.ssp.mydomain.tld` -> routes to `git.mydomain.tld/{user}/{repo}`
2. **Serving & Caching:** On receiving a request, `Forgejo_SSP` checks its local data directory:
    * **Cache Miss / Expired:** If the data is not present, or hasn't been refreshed in over 1 hour, the service clones the Git repo, switches to the configured pages branch, and serves the files.
    * **Cache Hit:** If the data is present and was refreshed within the last hour, the request is served instantly from the local cache.

## Configuration (Environment Variables)

Configure the application using the following environment variables.

| Variable | Description | Example |
| :--- | :--- | :--- |
| `HTTP_PORT` | The port the HTTP server will listen on. | `3000` |
| `HTTP_ADDR` | The address the HTTP server will bind to. | `0.0.0.0` |
| `HTTP_HOST_SUFFIX` | The base domain suffix to strip from incoming requests to determine the user and repo. | `.ssp.mydomain.tld` |
| `DATA` | The local directory path where cloned repositories (the cache) will be stored. | `/app/data` |
| `GIT_HTTPS_BASE_URL` | The base URL of your self-hosted Git server. | `https://git.mydomain.tld` |
| `GIT_PAGES_BRANCH` | The target branch the SSP will look for to serve static files. | `pages` or `gh-pages` |
| `GIT_DEFAULT_REPO_USER`| The default username/organization to use if one is not specified in the subdomain. | `my-org` |
| `GIT_PASSWORD` | The Git Personal Access Token (PAT) used to authenticate and pull private repositories. | `your_forgejo_token` |