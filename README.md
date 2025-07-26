# HuggingFaceDownloader

A tiny Huggingface repository downloader.

Download files from HuggingFace repository easily, support parallel files download. If url supports range, the file will be downloaded in chunks.

## Example
Download `models` `moonshotai/Kimi-K2-Instruct` with `4` files parallel at a time.
```
huggingfacedownloader -j moonshotai/Kimi-K2-Instruct -m 4
```
Download `datasets` `moonshotai/Kimi-K2-Instruct` with `4` files parallel at a time.
```
huggingfacedownloader -j facebook/flores -m 4 -t datasets
```
