# RR Channels
Experiment in bringing more determinism to message passing channels.

See http://arxiv.org/abs/1909.03111 for the full writeup. This is still very much WIP. There is currently a terrible performance bug for IPC Channels.

Our `cargo example`s can be record and replayed like so:
```
> # Recording execution to `record.log`
> RR_RECORD_FILE=record.log RR_MODE=record cargo run --example crossbeam
(0, 0)
(0, 1)
(4, 0)
(0, 2)
...
> # Replaying execution from same file
>  RR_RECORD_FILE=record.log RR_MODE=replay cargo run --example crossbeam
(0, 0)
(0, 1)
(4, 0)
(0, 2
```
