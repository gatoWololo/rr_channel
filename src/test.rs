/// Utility code for unit testing!
#[cfg(test)]
mod test {
    use crate::RRMode;
    use crate::{crossbeam_channel as cb, init_tivo_thread_root};
    use anyhow::Result;
    use std::error::Error;
    use std::thread;
    use std::time::Duration;

    /// Interface shared by channels. Allows our test programs to abstract over channel
    /// type.
    ///
    /// Have trait gone too far?
    trait TestChannel<T> {
        type S: Sender<T>;
        type R: Receiver<T>;
        fn make_channels(mode: RRMode) -> (Self::S, Self::R);
    }

    struct Crossbeam {}

    impl<T: 'static + Send + Sync> TestChannel<T> for Crossbeam {
        type S = cb::Sender<T>;
        type R = cb::Receiver<T>;

        fn make_channels(mode: RRMode) -> (Self::S, Self::R) {
            cb::unbounded_in_memory(mode)
        }
    }

    trait Sender<T> {
        type SendError: 'static + Error + Send + Sync;
        fn send(&self, msg: T) -> Result<(), Self::SendError>;
    }

    trait Receiver<T> {
        type RecvError: 'static + Error + Send + Sync;
        type TryRecvError: 'static + Error + Send + Sync;
        type RecvTimeoutError: 'static + Error + Send + Sync;

        fn recv(&self) -> Result<T, Self::RecvError>;
        fn try_recv(&self) -> Result<T, Self::TryRecvError>;

        const TIMEOUT: Self::RecvTimeoutError;
        const DISCONNECTED: Self::RecvTimeoutError;
    }

    impl<T: 'static + Send + Sync> Sender<T> for cb::Sender<T> {
        type SendError = cb::SendError<T>;
        fn send(&self, msg: T) -> Result<(), cb::SendError<T>> {
            self.send(msg)
        }
    }

    impl<T: 'static + Send + Sync> Receiver<T> for cb::Receiver<T> {
        type RecvError = cb::RecvError;
        type TryRecvError = cb::TryRecvError;
        type RecvTimeoutError = cb::RecvTimeoutError;

        fn recv(&self) -> Result<T, cb::RecvError> {
            self.recv()
        }

        fn try_recv(&self) -> Result<T, cb::TryRecvError> {
            self.try_recv()
        }

        const TIMEOUT: Self::RecvTimeoutError = cb::RecvTimeoutError::Timeout;
        const DISCONNECTED: Self::RecvTimeoutError = cb::RecvTimeoutError::Disconnected;
    }

    fn simple_program<C: TestChannel<i32>>(r: RRMode) -> Result<()> {
        // Program which we'll compare logger for.
        let (s, r) = C::make_channels(r);
        s.send(1)?;
        s.send(3)?;
        s.send(5)?;
        r.recv()?;
        r.recv()?;
        r.recv()?;
        Ok(())
    }

    fn recv_program<C: TestChannel<i32>>(r: RRMode) -> Result<()> {
        let (s, r) = C::make_channels(r);
        let _ = s.send(1)?;
        let _ = s.send(2)?;

        assert_eq!(r.recv()?, 1);
        assert_eq!(r.recv()?, 2);
        Ok(())
    }

    // fn recv_timeout_program<C: TestChannel<i32>>(r: RRMode) -> Result<()> {
    //     let (s, r) = C::make_channels(r);
    //
    //     let h = crate::detthread::spawn::<_, Result<()>>(move || {
    //         thread::sleep(Duration::from_millis(1));
    //         s.send(5)?;
    //         drop(s);
    //         Ok(())
    //     });
    //
    //     assert_eq!(
    //         r.recv_timeout(Duration::from_micros(500)),
    //         Err(C::R::TIMEOUT),
    //     );
    //     assert_eq!(r.recv_timeout(Duration::from_millis(1)), Ok(5));
    //     assert_eq!(r.recv_timeout(Duration::from_millis(1)), Err(C::R::TIMEOUT),);
    //
    //     // "Unlike with normal errors, this value doesn't implement the Error trait." So we unwrap
    //     // instead. Not sure why std::thread::Result doesn't impl Result...
    //     h.join().unwrap()?;
    //     Ok(())
    // }

    #[test]
    fn foo() {
        init_tivo_thread_root();
        simple_program::<Crossbeam>(RRMode::NoRR);
    }

    #[test]
    fn foo2() {
        init_tivo_thread_root();
        recv_program::<Crossbeam>(RRMode::NoRR);
    }
}
