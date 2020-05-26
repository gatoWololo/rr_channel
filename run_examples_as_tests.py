import os
import subprocess

import sys

trials = 30

# Pipes stderr to debug_logs file
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

# Swap out last line of function for uncommented line to display stderr.
def cargo_run_replay(example):
    env = os.environ.copy()
    env["RR_CHANNEL"] = "replay"
    env["RR_DESYNC_MODE"] = "panic"
    env["RR_RECORD_FILE"] = "record.log"
    return subprocess.check_output("cargo run --example " + example,
                                   env=env, shell=True, stderr=subprocess.DEVNULL)
                                   # env=env, shell=True)

def get_current_examples():
    # We're calling a partial cargo command, we expect it to fail and return the
    # avaliable examples.
    try:
        _ = subprocess.check_output(["cargo", "run", "--example"],
                                    stderr=subprocess.STDOUT)
        # Should jump to "except" case here!
        print('Running "cargo run --examples" was expected to fail...')
        sys.exit(1)
    except subprocess.CalledProcessError as e:
        output = e.output
        cargo_run_failed = True

    # print("Output: ", output)

    loop_done = False
    examples = []

    # Strip the whitespace from the stderr output and split by lines.
    for line in output.strip().split(b'\n'):
        # print(line)
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

def run_example(example):
    print("Running example: {}".format(example))

    # Run multiple times to be sure...
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
            # Some examples are expected to fail on replay.
            # This is fine.
            if "failure_expected" in example:
                continue
            else:
                print("Failure")
                sys.exit(1)
                return

        if record != replay:
            print("ERROR: Outputs between record and replay differ!")
            print("Record ", record)
            print("Replay ", replay)
            sys.exit(1)
    print("[{}/{}] OK!".format(trials, trials))


# Run all examples
if len(sys.argv) == 1:
    examples = get_current_examples()
    for example in examples:
        example = example.decode("utf-8")
        run_example(example)
elif len(sys.argv) == 2:
    run_example(sys.argv[1])
else:
     print("usage: python3 run_examples_as_test [example_name]")
