#!/usr/bin/python3
"""
Script responsible for compiling and running our example programs as tests. For every example in the examples/ directory,
We execute each example `trial` number of times. Comparing the output of the recorded execution vs the replayed execution
ensuring the results stay the same.
"""
import os
import subprocess
import difflib
import sys
from progress.bar import Bar

# TODO make this parametric via command line with default value.
trials = 30


# Record execution of example. This function pipes stderr to a "debug_logs" file.
def cargo_run_record(example):
    env = os.environ.copy()
    env["RR_CHANNEL"] = "record"
    env["RR_DESYNC_MODE"] = "panic"
    env["RR_RECORD_FILE"] = "record.log"
    # env["RUST_LOG"] = "warn"

    ferr = open("debug_logs", "w")
    return subprocess.check_output("cargo run --release --example " + example,
                                   # env=env, shell=True, stderr=subprocess.DEVNULL)
                                   env=env, shell=True, stderr=ferr)


# Executes one instance of our example using the "record.log" log file. Returns the output of this.
# Swap out last line of function for uncommented line to display stderr.
def cargo_run_replay(example):
    env = os.environ.copy()
    env["RR_CHANNEL"] = "replay"
    env["RR_DESYNC_MODE"] = "panic"
    env["RR_RECORD_FILE"] = "record.log"
    return subprocess.check_output("cargo run --example " + example,
                                   env=env, shell=True, stderr=subprocess.DEVNULL)
    # env=env, shell=True)


# Query cargo via the command line for all available examples to execute.
def get_current_examples():
    # We're calling a partial cargo command, we expect it to fail and return the
    # available examples.
    try:
        _ = subprocess.check_output(["cargo", "run", "--example"],
                                    stderr=subprocess.STDOUT)
        # Should jump to "except" case here!
        print('Error: Running "cargo run --examples" was expected to fail...')
        sys.exit(1)
    except subprocess.CalledProcessError as e:
        output = e.output
        cargo_run_failed = True

    # print("Output: ", output)

    loop_done = False
    examples = []

    # Strip the whitespace from the stderr output and split by lines.
    for line in output.strip().split(b'\n'):
        if line.strip() == b"Available examples:":
            loop_done = True
            continue
        if loop_done:
            examples.append(line.strip())

    if not loop_done:
        print('Running "cargo run --examples" did not return the expected output format')
        sys.exit(1)

    # print("Examples: ", examples)
    return examples


# Execute the given example `trial` number of times while outputting a progress bar of results.
def run_example(example):
    # Run multiple times to be sure...
    bar = Bar("Processing {}".format(example), max=trials)
    for _ in range(trials):
        record = ""
        replay = ""
        try:
            record = cargo_run_record(example)
        except:
            print("Failed to run example \'{}\' Maybe it doesn't exist?".format(example))
            return
        try:
            replay = cargo_run_replay(example)
        except:
            # Some examples are expected to fail on replay, so handle those here.
            if "failure_expected" in example:
                bar.next()
                continue
            else:
                print("Failure")
                sys.exit(1)
                return

        if record != replay:
            # From https://pymotw.com/2/difflib/
            d = difflib.Differ()
            diff = d.compare(record.decode().splitlines(), replay.decode().splitlines())
            print("ERROR: Outputs between record and replay differ!")
            print("\n".join(list(diff)))
            sys.exit(1)
        bar.next()
    bar.finish()


def main():
    if len(sys.argv) == 1:
        examples = get_current_examples()
        for example in examples:
            example = example.decode("utf-8")
            run_example(example)
    elif len(sys.argv) == 2:
        run_example(sys.argv[1])
    else:
        print("usage: python3 run_examples_as_test [example_name]")


if __name__ == "__main__":
    main()
