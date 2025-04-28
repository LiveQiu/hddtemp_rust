# hddtemp build by rust

## Introduce

- hddtemp-lt This project is slow to read the hard disk temperature, so I rewrite the functionality by Rust, and the entire reading function is based on smartctl.
- Thanks to all the authors for their open soiurce support.
- This project compiles components using linux-musl and aims to address cross-platform dependency issues as much as possible.

## How to use

- build

```console
make build
```

- run
```console
[root] make run
[user] sudo make run
```
