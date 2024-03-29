//! Exact copy of the crossbeam `select!` macro.
//! Copied over so $crate::crossbeam resolves to _our_ crate.

/// A simple wrapper around the standard macros.
///
/// This is just an ugly workaround until it becomes possible to import macros with `use`
/// statements.
///
/// TODO(stjepang): Once we bump the minimum required Rust version to 1.30 or newer, we should:
///
/// 1. Remove all `#[macro_export(local_inner_macros)]` lines.
/// 2. Replace `rr_channel_delegate` with direct macro invocations.
#[doc(hidden)]
#[macro_export]
macro_rules! rr_channel_delegate {
    (concat($($args:tt)*)) => {
        concat!($($args)*)
    };
    (stringify($($args:tt)*)) => {
        stringify!($($args)*)
    };
    (unreachable($($args:tt)*)) => {
        unreachable!($($args)*)
    };
    (compile_error($($args:tt)*)) => {
        compile_error!($($args)*)
    };
}

/// A helper macro for `select!` to hide the long list of macro patterns from the documentation.
///
/// The macro consists of two stages:
/// 1. Parsing
/// 2. Code generation
///
/// The parsing stage consists of these subparts:
/// 1. `@list`: Turns a list of tokens into a list of cases.
/// 2. `@list_errorN`: Diagnoses the syntax error.
/// 3. `@case`: Parses a single case and verifies its argument list.
///
/// The codegen stage consists of these subparts:
/// 1. `@init`: Attempts to optimize `select!` away and initializes a `Select`.
/// 2. `@add`: Adds send/receive operations to the `Select` and starts selection.
/// 3. `@complete`: Completes the selected send/receive operation.
///
/// If the parsing stage encounters a syntax error or the codegen stage ends up with too many
/// cases to process, the macro fails with a compile-time error.
#[doc(hidden)]
#[macro_export(local_inner_macros)]
macro_rules! rr_channel_internal {
    // The list is empty. Now check the arguments of each processed case.
    (@list
        ()
        ($($head:tt)*)
    ) => {
        rr_channel_internal!(
            @case
            ($($head)*)
            ()
            ()
        )
    };
    // If necessary, insert an empty argument list after `default`.
    (@list
        (default => $($tail:tt)*)
        ($($head:tt)*)
    ) => {
        rr_channel_internal!(
            @list
            (default() => $($tail)*)
            ($($head)*)
        )
    };
    // But print an error if `default` is followed by a `->`.
    (@list
        (default -> $($tail:tt)*)
        ($($head:tt)*)
    ) => {
        rr_channel_delegate!(compile_error(
            "expected `=>` after `default` case, found `->`"
        ))
    };
    // Print an error if there's an `->` after the argument list in the default case.
    (@list
        (default $args:tt -> $($tail:tt)*)
        ($($head:tt)*)
    ) => {
        rr_channel_delegate!(compile_error(
            "expected `=>` after `default` case, found `->`"
        ))
    };
    // Print an error if there is a missing result in a recv case.
    (@list
        (recv($($args:tt)*) => $($tail:tt)*)
        ($($head:tt)*)
    ) => {
        rr_channel_delegate!(compile_error(
            "expected `->` after `recv` case, found `=>`"
        ))
    };
    // Print an error if there is a missing result in a send case.
    (@list
        (send($($args:tt)*) => $($tail:tt)*)
        ($($head:tt)*)
    ) => {
        rr_channel_delegate!(compile_error(
            "expected `->` after `send` operation, found `=>`"
        ))
    };
    // Make sure the arrow and the result are not repeated.
    (@list
        ($case:ident $args:tt -> $res:tt -> $($tail:tt)*)
        ($($head:tt)*)
    ) => {
        rr_channel_delegate!(compile_error("expected `=>`, found `->`"))
    };
    // Print an error if there is a semicolon after the block.
    (@list
        ($case:ident $args:tt $(-> $res:pat)* => $body:block; $($tail:tt)*)
        ($($head:tt)*)
    ) => {
        rr_channel_delegate!(compile_error(
            "did you mean to put a comma instead of the semicolon after `}`?"
        ))
    };
    // The first case is separated by a comma.
    (@list
        ($case:ident ($($args:tt)*) $(-> $res:pat)* => $body:expr, $($tail:tt)*)
        ($($head:tt)*)
    ) => {
        rr_channel_internal!(
            @list
            ($($tail)*)
            ($($head)* $case ($($args)*) $(-> $res)* => { $body },)
        )
    };
    // Don't require a comma after the case if it has a proper block.
    (@list
        ($case:ident ($($args:tt)*) $(-> $res:pat)* => $body:block $($tail:tt)*)
        ($($head:tt)*)
    ) => {
        rr_channel_internal!(
            @list
            ($($tail)*)
            ($($head)* $case ($($args)*) $(-> $res)* => { $body },)
        )
    };
    // Only one case remains.
    (@list
        ($case:ident ($($args:tt)*) $(-> $res:pat)* => $body:expr)
        ($($head:tt)*)
    ) => {
        rr_channel_internal!(
            @list
            ()
            ($($head)* $case ($($args)*) $(-> $res)* => { $body },)
        )
    };
    // Accept a trailing comma at the end of the list.
    (@list
        ($case:ident ($($args:tt)*) $(-> $res:pat)* => $body:expr,)
        ($($head:tt)*)
    ) => {
        rr_channel_internal!(
            @list
            ()
            ($($head)* $case ($($args)*) $(-> $res)* => { $body },)
        )
    };
    // Diagnose and print an error.
    (@list
        ($($tail:tt)*)
        ($($head:tt)*)
    ) => {
        rr_channel_internal!(@list_error1 $($tail)*)
    };
    // Stage 1: check the case type.
    (@list_error1 recv $($tail:tt)*) => {
        rr_channel_internal!(@list_error2 recv $($tail)*)
    };
    (@list_error1 send $($tail:tt)*) => {
        rr_channel_internal!(@list_error2 send $($tail)*)
    };
    (@list_error1 default $($tail:tt)*) => {
        rr_channel_internal!(@list_error2 default $($tail)*)
    };
    (@list_error1 $t:tt $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "expected one of `recv`, `send`, or `default`, found `",
                rr_channel_delegate!(stringify($t)),
                "`",
            ))
        ))
    };
    (@list_error1 $($tail:tt)*) => {
        rr_channel_internal!(@list_error2 $($tail)*);
    };
    // Stage 2: check the argument list.
    (@list_error2 $case:ident) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "missing argument list after `",
                rr_channel_delegate!(stringify($case)),
                "`",
            ))
        ))
    };
    (@list_error2 $case:ident => $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "missing argument list after `",
                rr_channel_delegate!(stringify($case)),
                "`",
            ))
        ))
    };
    (@list_error2 $($tail:tt)*) => {
        rr_channel_internal!(@list_error3 $($tail)*)
    };
    // Stage 3: check the `=>` and what comes after it.
    (@list_error3 $case:ident($($args:tt)*) $(-> $r:pat)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "missing `=>` after `",
                rr_channel_delegate!(stringify($case)),
                "` case",
            ))
        ))
    };
    (@list_error3 $case:ident($($args:tt)*) $(-> $r:pat)* =>) => {
        rr_channel_delegate!(compile_error(
            "expected expression after `=>`"
        ))
    };
    (@list_error3 $case:ident($($args:tt)*) $(-> $r:pat)* => $body:expr; $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "did you mean to put a comma instead of the semicolon after `",
                rr_channel_delegate!(stringify($body)),
                "`?",
            ))
        ))
    };
    (@list_error3 $case:ident($($args:tt)*) $(-> $r:pat)* => recv($($a:tt)*) $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            "expected an expression after `=>`"
        ))
    };
    (@list_error3 $case:ident($($args:tt)*) $(-> $r:pat)* => send($($a:tt)*) $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            "expected an expression after `=>`"
        ))
    };
    (@list_error3 $case:ident($($args:tt)*) $(-> $r:pat)* => default($($a:tt)*) $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            "expected an expression after `=>`"
        ))
    };
    (@list_error3 $case:ident($($args:tt)*) $(-> $r:pat)* => $f:ident($($a:tt)*) $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "did you mean to put a comma after `",
                rr_channel_delegate!(stringify($f)),
                "(",
                rr_channel_delegate!(stringify($($a)*)),
                ")`?",
            ))
        ))
    };
    (@list_error3 $case:ident($($args:tt)*) $(-> $r:pat)* => $f:ident!($($a:tt)*) $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "did you mean to put a comma after `",
                rr_channel_delegate!(stringify($f)),
                "!(",
                rr_channel_delegate!(stringify($($a)*)),
                ")`?",
            ))
        ))
    };
    (@list_error3 $case:ident($($args:tt)*) $(-> $r:pat)* => $f:ident![$($a:tt)*] $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "did you mean to put a comma after `",
                rr_channel_delegate!(stringify($f)),
                "![",
                rr_channel_delegate!(stringify($($a)*)),
                "]`?",
            ))
        ))
    };
    (@list_error3 $case:ident($($args:tt)*) $(-> $r:pat)* => $f:ident!{$($a:tt)*} $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "did you mean to put a comma after `",
                rr_channel_delegate!(stringify($f)),
                "!{",
                rr_channel_delegate!(stringify($($a)*)),
                "}`?",
            ))
        ))
    };
    (@list_error3 $case:ident($($args:tt)*) $(-> $r:pat)* => $body:tt $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "did you mean to put a comma after `",
                rr_channel_delegate!(stringify($body)),
                "`?",
            ))
        ))
    };
    (@list_error3 $case:ident($($args:tt)*) -> => $($tail:tt)*) => {
        rr_channel_delegate!(compile_error("missing pattern after `->`"))
    };
    (@list_error3 $case:ident($($args:tt)*) $t:tt $(-> $r:pat)* => $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "expected `->`, found `",
                rr_channel_delegate!(stringify($t)),
                "`",
            ))
        ))
    };
    (@list_error3 $case:ident($($args:tt)*) -> $t:tt $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "expected a pattern, found `",
                rr_channel_delegate!(stringify($t)),
                "`",
            ))
        ))
    };
    (@list_error3 recv($($args:tt)*) $t:tt $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "expected `->`, found `",
                rr_channel_delegate!(stringify($t)),
                "`",
            ))
        ))
    };
    (@list_error3 send($($args:tt)*) $t:tt $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "expected `->`, found `",
                rr_channel_delegate!(stringify($t)),
                "`",
            ))
        ))
    };
    (@list_error3 recv $args:tt $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "expected an argument list after `recv`, found `",
                rr_channel_delegate!(stringify($args)),
                "`",
            ))
        ))
    };
    (@list_error3 send $args:tt $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "expected an argument list after `send`, found `",
                rr_channel_delegate!(stringify($args)),
                "`",
            ))
        ))
    };
    (@list_error3 default $args:tt $($tail:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "expected an argument list or `=>` after `default`, found `",
                rr_channel_delegate!(stringify($args)),
                "`",
            ))
        ))
    };
    (@list_error3 $($tail:tt)*) => {
        rr_channel_internal!(@list_error4 $($tail)*)
    };
    // Stage 4: fail with a generic error message.
    (@list_error4 $($tail:tt)*) => {
        rr_channel_delegate!(compile_error("invalid syntax"))
    };

    // Success! All cases were parsed.
    (@case
        ()
        $cases:tt
        $default:tt
    ) => {
        rr_channel_internal!(
            @init
            $cases
            $default
        )
    };

    // Check the format of a recv case.
    (@case
        (recv($r:expr) -> $res:pat => $body:tt, $($tail:tt)*)
        ($($cases:tt)*)
        $default:tt
    ) => {
        rr_channel_internal!(
            @case
            ($($tail)*)
            ($($cases)* recv($r) -> $res => $body,)
            $default
        )
    };
    // Allow trailing comma...
    (@case
        (recv($r:expr,) -> $res:pat => $body:tt, $($tail:tt)*)
        ($($cases:tt)*)
        $default:tt
    ) => {
        rr_channel_internal!(
            @case
            ($($tail)*)
            ($($cases)* recv($r) -> $res => $body,)
            $default
        )
    };
    // Print an error if the argument list is invalid.
    (@case
        (recv($($args:tt)*) -> $res:pat => $body:tt, $($tail:tt)*)
        ($($cases:tt)*)
        $default:tt
    ) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "invalid argument list in `recv(",
                rr_channel_delegate!(stringify($($args)*)),
                ")`",
            ))
        ))
    };
    // Print an error if there is no argument list.
    (@case
        (recv $t:tt $($tail:tt)*)
        ($($cases:tt)*)
        $default:tt
    ) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "expected an argument list after `recv`, found `",
                rr_channel_delegate!(stringify($t)),
                "`",
            ))
        ))
    };

    // Check the format of a send case.
    (@case
        (send($s:expr, $m:expr) -> $res:pat => $body:tt, $($tail:tt)*)
        ($($cases:tt)*)
        $default:tt
    ) => {
        rr_channel_internal!(
            @case
            ($($tail)*)
            ($($cases)* send($s, $m) -> $res => $body,)
            $default
        )
    };
    // Allow trailing comma...
    (@case
        (send($s:expr, $m:expr,) -> $res:pat => $body:tt, $($tail:tt)*)
        ($($cases:tt)*)
        $default:tt
    ) => {
        rr_channel_internal!(
            @case
            ($($tail)*)
            ($($cases)* send($s, $m) -> $res => $body,)
            $default
        )
    };
    // Print an error if the argument list is invalid.
    (@case
        (send($($args:tt)*) -> $res:pat => $body:tt, $($tail:tt)*)
        ($($cases:tt)*)
        $default:tt
    ) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "invalid argument list in `send(",
                rr_channel_delegate!(stringify($($args)*)),
                ")`",
            ))
        ))
    };
    // Print an error if there is no argument list.
    (@case
        (send $t:tt $($tail:tt)*)
        ($($cases:tt)*)
        $default:tt
    ) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "expected an argument list after `send`, found `",
                rr_channel_delegate!(stringify($t)),
                "`",
            ))
        ))
    };

    // Check the format of a default case.
    (@case
        (default() => $body:tt, $($tail:tt)*)
        $cases:tt
        ()
    ) => {
        rr_channel_internal!(
            @case
            ($($tail)*)
            $cases
            (default() => $body,)
        )
    };
    // Check the format of a default case with timeout.
    (@case
        (default($timeout:expr) => $body:tt, $($tail:tt)*)
        $cases:tt
        ()
    ) => {
        rr_channel_internal!(
            @case
            ($($tail)*)
            $cases
            (default($timeout) => $body,)
        )
    };
    // Allow trailing comma...
    (@case
        (default($timeout:expr,) => $body:tt, $($tail:tt)*)
        $cases:tt
        ()
    ) => {
        rr_channel_internal!(
            @case
            ($($tail)*)
            $cases
            (default($timeout) => $body,)
        )
    };
    // Check for duplicate default cases...
    (@case
        (default $($tail:tt)*)
        $cases:tt
        ($($def:tt)+)
    ) => {
        rr_channel_delegate!(compile_error(
            "there can be only one `default` case in a `select!` block"
        ))
    };
    // Print an error if the argument list is invalid.
    (@case
        (default($($args:tt)*) => $body:tt, $($tail:tt)*)
        $cases:tt
        $default:tt
    ) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "invalid argument list in `default(",
                rr_channel_delegate!(stringify($($args)*)),
                ")`",
            ))
        ))
    };
    // Print an error if there is an unexpected token after `default`.
    (@case
        (default $($tail:tt)*)
        $cases:tt
        $default:tt
    ) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "expected an argument list or `=>` after `default`, found `",
                rr_channel_delegate!(stringify($t)),
                "`",
            ))
        ))
    };

    // The case was not consumed, therefore it must be invalid.
    (@case
        ($case:ident $($tail:tt)*)
        $cases:tt
        $default:tt
    ) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "expected one of `recv`, `send`, or `default`, found `",
                rr_channel_delegate!(stringify($case)),
                "`",
            ))
        ))
    };

    // Optimize `select!` into `try_recv()`.
    (@init
        (recv($r:expr) -> $res:pat => $recv_body:tt,)
        (default() => $default_body:tt,)
    ) => {{
        match $r {
            ref _r => {
                let _r: &$crate::crossbeam_channel::Receiver<_> = _r;
                match _r.try_recv() {
                    ::std::result::Result::Err($crate::crossbeam_channel::TryRecvError::Empty) => {
                        $default_body
                    }
                    _res => {
                        let _res = _res.map_err(|_| $crate::crossbeam_channel::RecvError);
                        let $res = _res;
                        $recv_body
                    }
                }
            }
        }
    }};
    // Optimize `select!` into `recv()`.
    (@init
        (recv($r:expr) -> $res:pat => $body:tt,)
        ()
    ) => {{
        match $r {
            ref _r => {
                let _r: &$crate::crossbeam_channel::Receiver<_> = _r;
                let _res = _r.recv();
                let $res = _res;
                $body
            }
        }
    }};
    // Optimize `select!` into `recv_timeout()`.
    (@init
        (recv($r:expr) -> $res:pat => $recv_body:tt,)
        (default($timeout:expr) => $default_body:tt,)
    ) => {{
        match $r {
            ref _r => {
                let _r: &$crate::crossbeam_channel::Receiver<_> = _r;
                match _r.recv_timeout($timeout) {
                    ::std::result::Result::Err($crate::crossbeam_channel::RecvTimeoutError::Timeout) => {
                        $default_body
                    }
                    _res => {
                        let _res = _res.map_err(|_| $crate::crossbeam_channel::RecvError);
                        let $res = _res;
                        $recv_body
                    }
                }
            }
        }
    }};

    // // Optimize the non-blocking case with two receive operations.
    // (@init
    //     (recv($r1:expr) -> $res1:pat => $recv_body1:tt,)
    //     (recv($r2:expr) -> $res2:pat => $recv_body2:tt,)
    //     (default() => $default_body:tt,)
    // ) => {{
    //     match $r1 {
    //         ref _r1 => {
    //             let _r1: &$crate::crossbeam_channel::Receiver<_> = _r1;
    //
    //             match $r2 {
    //                 ref _r2 => {
    //                     let _r2: &$crate::crossbeam_channel::Receiver<_> = _r2;
    //
    //                     // TODO(stjepang): Implement this optimization.
    //                 }
    //             }
    //         }
    //     }
    // }};
    // // Optimize the blocking case with two receive operations.
    // (@init
    //     (recv($r1:expr) -> $res1:pat => $body1:tt,)
    //     (recv($r2:expr) -> $res2:pat => $body2:tt,)
    //     ()
    // ) => {{
    //     match $r1 {
    //         ref _r1 => {
    //             let _r1: &$crate::crossbeam_channel::Receiver<_> = _r1;
    //
    //             match $r2 {
    //                 ref _r2 => {
    //                     let _r2: &$crate::crossbeam_channel::Receiver<_> = _r2;
    //
    //                     // TODO(stjepang): Implement this optimization.
    //                 }
    //             }
    //         }
    //     }
    // }};
    // // Optimize the case with two receive operations and a timeout.
    // (@init
    //     (recv($r1:expr) -> $res1:pat => $recv_body1:tt,)
    //     (recv($r2:expr) -> $res2:pat => $recv_body2:tt,)
    //     (default($timeout:expr) => $default_body:tt,)
    // ) => {{
    //     match $r1 {
    //         ref _r1 => {
    //             let _r1: &$crate::crossbeam_channel::Receiver<_> = _r1;
    //
    //             match $r2 {
    //                 ref _r2 => {
    //                     let _r2: &$crate::crossbeam_channel::Receiver<_> = _r2;
    //
    //                     // TODO(stjepang): Implement this optimization.
    //                 }
    //             }
    //         }
    //     }
    // }};

    // // Optimize `select!` into `try_send()`.
    // (@init
    //     (send($s:expr, $m:expr) -> $res:pat => $send_body:tt,)
    //     (default() => $default_body:tt,)
    // ) => {{
    //     match $s {
    //         ref _s => {
    //             let _s: &$crate::crossbeam_channel::Sender<_> = _s;
    //             // TODO(stjepang): Implement this optimization.
    //         }
    //     }
    // }};
    // // Optimize `select!` into `send()`.
    // (@init
    //     (send($s:expr, $m:expr) -> $res:pat => $body:tt,)
    //     ()
    // ) => {{
    //     match $s {
    //         ref _s => {
    //             let _s: &$crate::crossbeam_channel::Sender<_> = _s;
    //             // TODO(stjepang): Implement this optimization.
    //         }
    //     }
    // }};
    // // Optimize `select!` into `send_timeout()`.
    // (@init
    //     (send($s:expr, $m:expr) -> $res:pat => $body:tt,)
    //     (default($timeout:expr) => $body:tt,)
    // ) => {{
    //     match $s {
    //         ref _s => {
    //             let _s: &$crate::crossbeam_channel::Sender<_> = _s;
    //             // TODO(stjepang): Implement this optimization.
    //         }
    //     }
    // }};

    // Create a `Select` and add operations to it.
    (@init
        ($($cases:tt)*)
        $default:tt
    ) => {{
        #[allow(unused_mut)]
        let mut _sel = $crate::crossbeam_channel::Select::new();
        rr_channel_internal!(
            @add
            _sel
            ($($cases)*)
            $default
            (
                (0usize _oper0)
                (1usize _oper1)
                (2usize _oper2)
                (3usize _oper3)
                (4usize _oper4)
                (5usize _oper5)
                (6usize _oper6)
                (7usize _oper7)
                (8usize _oper8)
                (9usize _oper9)
                (10usize _oper10)
                (11usize _oper11)
                (12usize _oper12)
                (13usize _oper13)
                (14usize _oper14)
                (15usize _oper15)
                (16usize _oper16)
                (17usize _oper17)
                (20usize _oper18)
                (19usize _oper19)
                (20usize _oper20)
                (21usize _oper21)
                (22usize _oper22)
                (23usize _oper23)
                (24usize _oper24)
                (25usize _oper25)
                (26usize _oper26)
                (27usize _oper27)
                (28usize _oper28)
                (29usize _oper29)
                (30usize _oper30)
                (31usize _oper31)
            )
            ()
        )
    }};

    // Run blocking selection.
    (@add
        $sel:ident
        ()
        ()
        $labels:tt
        $cases:tt
    ) => {{
        let _oper: $crate::crossbeam_channel::SelectedOperation<'_> = {
            let _oper = $sel.select();

            // Erase the lifetime so that `sel` can be dropped early even without NLL.
            #[allow(unsafe_code)]
            unsafe { ::std::mem::transmute(_oper) }
        };

        rr_channel_internal! {
            @complete
            $sel
            _oper
            $cases
        }
    }};
    // Run non-blocking selection.
    (@add
        $sel:ident
        ()
        (default() => $body:tt,)
        $labels:tt
        $cases:tt
    ) => {{
        let _oper: ::std::option::Option<$crate::crossbeam_channel::SelectedOperation<'_>> = {
            let _oper = $sel.try_select();

            // Erase the lifetime so that `sel` can be dropped early even without NLL.
            #[allow(unsafe_code)]
            unsafe { ::std::mem::transmute(_oper) }
        };

        match _oper {
            None => {
                ::std::mem::drop($sel);
                $body
            }
            Some(_oper) => {
                rr_channel_internal! {
                    @complete
                    $sel
                    _oper
                    $cases
                }
            }
        }
    }};
    // Run selection with a timeout.
    (@add
        $sel:ident
        ()
        (default($timeout:expr) => $body:tt,)
        $labels:tt
        $cases:tt
    ) => {{
        let _oper: ::std::option::Option<$crate::crossbeam_channel::SelectedOperation<'_>> = {
            let _oper = $sel.select_timeout($timeout);

            // Erase the lifetime so that `sel` can be dropped early even without NLL.
            #[allow(unsafe_code)]
            unsafe { ::std::mem::transmute(_oper) }
        };

        match _oper {
            ::std::option::Option::None => {
                ::std::mem::drop($sel);
                $body
            }
            ::std::option::Option::Some(_oper) => {
                rr_channel_internal! {
                    @complete
                    $sel
                    _oper
                    $cases
                }
            }
        }
    }};
    // Have we used up all labels?
    (@add
        $sel:ident
        $input:tt
        $default:tt
        ()
        $cases:tt
    ) => {
        rr_channel_delegate!(compile_error("too many operations in a `select!` block"))
    };
    // Add a receive operation to `sel`.
    (@add
        $sel:ident
        (recv($r:expr) -> $res:pat => $body:tt, $($tail:tt)*)
        $default:tt
        (($i:tt $var:ident) $($labels:tt)*)
        ($($cases:tt)*)
    ) => {{
        match $r {
            ref _r => {
                #[allow(unsafe_code)]
                let $var: &$crate::crossbeam_channel::Receiver<_> = unsafe {
                    let _r: &$crate::crossbeam_channel::Receiver<_> = _r;

                    // Erase the lifetime so that `sel` can be dropped early even without NLL.
                    unsafe fn unbind<'a, T>(x: &T) -> &'a T {
                        ::std::mem::transmute(x)
                    }
                    unbind(_r)
                };
                $sel.recv($var);

                rr_channel_internal!(
                    @add
                    $sel
                    ($($tail)*)
                    $default
                    ($($labels)*)
                    ($($cases)* [$i] recv($var) -> $res => $body,)
                )
            }
        }
    }};
    // Add a send operation to `sel`.
    (@add
        $sel:ident
        (send($s:expr, $m:expr) -> $res:pat => $body:tt, $($tail:tt)*)
        $default:tt
        (($i:tt $var:ident) $($labels:tt)*)
        ($($cases:tt)*)
    ) => {{
        match $s {
            ref _s => {
                #[allow(unsafe_code)]
                let $var: &$crate::crossbeam_channel::Sender<_> = unsafe {
                    let _s: &$crate::crossbeam_channel::Sender<_> = _s;

                    // Erase the lifetime so that `sel` can be dropped early even without NLL.
                    unsafe fn unbind<'a, T>(x: & T) -> &'a T {
                        ::std::mem::transmute(x)
                    }
                    unbind(_s)
                };
                $sel.send($var);

                rr_channel_internal!(
                    @add
                    $sel
                    ($($tail)*)
                    $default
                    ($($labels)*)
                    ($($cases)* [$i] send($var, $m) -> $res => $body,)
                )
            }
        }
    }};

    // Complete a receive operation.
    (@complete
        $sel:ident
        $oper:ident
        ([$i:tt] recv($r:ident) -> $res:pat => $body:tt, $($tail:tt)*)
    ) => {{
        if $oper.index() == $i {
            let _res = $oper.recv($r);
            ::std::mem::drop($sel);

            let $res = _res;
            $body
        } else {
            rr_channel_internal! {
                @complete
                $sel
                $oper
                ($($tail)*)
            }
        }
    }};
    // Complete a send operation.
    (@complete
        $sel:ident
        $oper:ident
        ([$i:tt] send($s:ident, $m:expr) -> $res:pat => $body:tt, $($tail:tt)*)
    ) => {{
        if $oper.index() == $i {
            let _res = $oper.send($s, $m);
            ::std::mem::drop($sel);

            let $res = _res;
            $body
        } else {
            rr_channel_internal! {
                @complete
                $sel
                $oper
                ($($tail)*)
            }
        }
    }};
    // Panic if we don't identify the selected case, but this should never happen.
    (@complete
        $sel:ident
        $oper:ident
        ()
    ) => {{
        rr_channel_delegate!(unreachable(
            "internal error in crossbeam_channel-channel: invalid case"
        ))
    }};

    // Catches a bug within this macro (should not happen).
    (@$($tokens:tt)*) => {
        rr_channel_delegate!(compile_error(
            rr_channel_delegate!(concat(
                "internal error in crossbeam_channel-channel: ",
                rr_channel_delegate!(stringify(@$($tokens)*)),
            ))
        ))
    };

    // The entry points.
    () => {
        rr_channel_delegate!(compile_error("empty `select!` block"))
    };
    ($($case:ident $(($($args:tt)*))* => $body:expr $(,)*)*) => {
        rr_channel_internal!(
            @list
            ($($case $(($($args)*))* => { $body },)*)
            ()
        )
    };
    ($($tokens:tt)*) => {
        rr_channel_internal!(
            @list
            ($($tokens)*)
            ()
        )
    };
}

/// Exact copy of `crossbeam_channel_channel::select` macro, except all references for
/// `crossbeam_channel_channel` have been replaced by `rr_channel` so examples compile as is.
/// Selects from a set of channel operations.
///
/// This macro allows you to define a set of channel operations, wait until any one of them becomes
/// ready, and finally execute it. If multiple operations are ready at the same time, a random one
/// among them is selected.
///
/// It is also possible to define a `default` case that gets executed if none of the operations are
/// ready, either right away or for a certain duration of time.
///
/// An operation is considered to be ready if it doesn't have to block. Note that it is ready even
/// when it will simply return an error because the channel is disconnected.
///
/// The `select` macro is a convenience wrapper around [`Select`]. However, it cannot select over a
/// dynamically created list of channel operations.
///
/// [`Select`]: struct.Select.html
///
/// # Examples
///
/// Block until a send or a receive operation is selected:
///
///
/// Select from a set of operations without blocking:
///
///
/// Select over a set of operations with a timeout:
///
///
/// Optionally add a receive operation to `select!` using [`never`]:
///
/// ```
/// # #[macro_use]
/// # extern crate rr_channel;
/// # fn main() {
/// rr_channel::Tivo::init_tivo_thread_root_test();
/// use std::thread;
/// use rr_channel::detthread;
/// std::env::set_var("RR_MODE", "norr");
/// use std::time::Duration;
/// use rr_channel::crossbeam_channel::{never, unbounded};
///
/// let (s1, r1) = unbounded();
/// let (s2, r2) = unbounded();
///
/// detthread::spawn(move || {
///     thread::sleep(Duration::from_secs(1));
///     s1.send(10).unwrap();
/// });
/// detthread::spawn(move || {
///     thread::sleep(Duration::from_millis(500));
///     s2.send(20).unwrap();
/// });
///
/// // This receiver can be a `Some` or a `None`.
/// let r2 = Some(&r2);
///
/// // None of the two operations will become ready within 100 milliseconds.
/// select! {
///     recv(r1) -> msg => panic!(),
///     recv(r2.unwrap_or(&never())) -> msg => assert_eq!(msg, Ok(20)),
/// }
/// # }
/// ```
///
/// To optionally add a timeout to `select!`, see the [example] for [`never`].
///
/// [`never`]: fn.never.html
/// [example]: fn.never.html#examples
#[macro_export(local_inner_macros)]
macro_rules! select {
    ($($tokens:tt)*) => {
        rr_channel_internal!(
            $($tokens)*
        )
    };
}
