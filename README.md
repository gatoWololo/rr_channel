t# RR Channels
Experiment in bringing more determinism to message passing channels.

See http://arxiv.org/abs/1909.03111 for the full writeup. This is still very much WIP. There is currently a terrible performance bug for IPC Channels.

Both examples `channel_select_no_disconnect.rs` and `channel_select_disconnect.rs` can be record and replayed:
```
> RR_CHANNEL=record cargo run --example channel_select
receiver 1: Ok(1)
receiver 1: Ok(1)
receiver 1: Ok(1)
receiver 2: Ok(2)
receiver 1: Ok(1)
receiver 1: Ok(1)
receiver 2: Ok(2)
receiver 2: Ok(2)
receiver 1: Ok(1)
receiver 1: Ok(1)
receiver 2: Ok(2)
receiver 2: Ok(2)
> RR_CHANNEL=replay cargo run --example channel_select
receiver 1: Ok(1)
receiver 1: Ok(1)
receiver 1: Ok(1)
receiver 2: Ok(2)
receiver 1: Ok(1)
receiver 1: Ok(1)
receiver 2: Ok(2)
receiver 2: Ok(2)
receiver 1: Ok(1)
receiver 1: Ok(1)
receiver 2: Ok(2)
receiver 2: Ok(2)
```
