# HuggingFaceDownloader

A tiny Huggingface repository downloader.

Download files from HuggingFace repository easily, support parallel files download by default `1` file at a time, to override use `-m 2` for 2 files at a time.
If url supports range, the file will be downloaded parallel in chunks by default `4` chunks, to override use `-c 1` for 1 chunk.

Downloader support download of HuggingFace `datasets` and `spaces` other than the default `models`, to override use `-t datasets` for datasets and `-t spaces` for spaces.

## Example
Download `models` `moonshotai/Kimi-K2-Instruct` with `4` files parallel at a time.
```
huggingfacedownloader -j moonshotai/Kimi-K2-Instruct -m 4
```
Download `datasets` `facebook/flores` with `2` files parallel at a time.
```
huggingfacedownloader -j facebook/flores -m 2 -t datasets
```
Download `models` `moonshotai/Kimi-K2-Instruct` with `1` files parallel at a time with each file `4` chunks.
```
huggingfacedownloader -j moonshotai/Kimi-K2-Instruct -m 1 -c 4
```
