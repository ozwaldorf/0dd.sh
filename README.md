# no bullshit command line pastebin

[![Mozilla HTTP Observatory Grade](https://img.shields.io/mozilla-observatory/grade-score/upld.is?style=for-the-badge)](https://developer.mozilla.org/en-US/observatory/analyze?host=upld.is)
[![get upld.is](https://img.shields.io/website?url=https%3A%2F%2Fupld.is&style=for-the-badge&label=GET%20upld.is)](https://upld.is)
[![get taa.gg](https://img.shields.io/website?url=https%3A%2F%2Ftaa.gg&style=for-the-badge&label=GET%20taa.gg)](https://taa.gg)
[![fastly deploy](https://img.shields.io/github/actions/workflow/status/ozwaldorf/upld.is/fastly.yaml?style=for-the-badge&label=CI%2FCD)](https://github.com/ozwaldorf/upld.is/actions/workflows/fastly.yaml)
[![uploads](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fupld.is%2Fjson&query=%24.uploads&style=for-the-badge&label=uploads)](https://upld.is/json)

## Usage

```
# Upload file
curl upld.is -LT filename

# Upload command output
command | curl upld.is -LT -

# Get content
curl https://upld.is/xxyyzzaa
```

## Development

> Note: the key value store must be created and assigned for uploads to work

```
fastly compute build
fastly compute deploy
fastly kv-store list
fastly kv-store-entry describe -qs <id> -k _upload_metrics
```
