# RR Channels
Experiment in converting bringing deterministic select to crossbeam channels.

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
