# [0dd.sh](https://0dd.sh) 

[![fastly deploy](https://img.shields.io/github/actions/workflow/status/ozwaldorf/0dd.sh/fastly.yaml?style=for-the-badge&label=CI%2FCD)](https://github.com/ozwaldorf/0dd.sh/actions/workflows/fastly.yaml)
[![get](https://img.shields.io/website?url=https%3A%2F%2F0dd.sh&label=0dd.sh&style=for-the-badge)](https://0dd.sh)
[![uploads](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2F0dd.sh%2Fjson&query=%24.uploads&style=for-the-badge&label=uploads)](https://0dd.sh/json)
[![Mozilla HTTP Observatory Grade](https://img.shields.io/mozilla-observatory/grade-score/0dd.sh?style=for-the-badge)](https://developer.mozilla.org/en-US/observatory/analyze?host=0dd.sh)

no bullshit command line pastebin

## Usage

```
# Upload file
curl 0dd.sh -LT filename

# Upload command output
command | curl 0dd.sh -LT -

# Get content
curl https://0dd.sh/xxyyzzaa

# Markdown to html
curl https://0dd.sh/deadbeef?md
```

## Development

> Note: the key value store must be created and assigned for uploads to work

```
fastly compute build
fastly compute deploy
fastly kv-store list
fastly kv-store-entry describe -qs <id> -k _upload_metrics
```
