# upld.is

no bullshit command line pastebin [upld.is](https://upld.is)

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
