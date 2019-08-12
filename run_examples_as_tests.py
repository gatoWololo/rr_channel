import os
import subprocess

import sys

def cargo_run_record(example):
    env = os.environ.copy()
    env["RR_CHANNEL"] = "record"
    env["RR_DESYNC_MODE"] = "panic"
    env["RR_RECORD_FILE"] = "record.log"
    return subprocess.check_output("cargo run --example " + example,
                                   env=env, shell=True, stderr=subprocess.DEVNULL)

def cargo_run_replay(example):
    env = os.environ.copy()
    env["RR_CHANNEL"] = "replay"
    env["RR_DESYNC_MODE"] = "keep_going"
    env["RR_RECORD_FILE"] = "record.log"
    return subprocess.check_output("cargo run --example " + example,
                                   env=env, shell=True, stderr=subprocess.DEVNULL)

# We're calling a partial cargo command, we expect it to fail and return the
# avaliable exampes.
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

for example in examples:
    example = example.decode("utf-8")
    print("Running example: {}".format(example))

    # Run multiple times to be sure...
    for _ in range(10):
        record = cargo_run_record(example)

        try:
            replay = cargo_run_replay(example)
        except:
            # Some examples are expected to fail on replay.
            # This is fine.
            if "failure_expected" in example:
                continue
            else:
                print("Failure")

        if record != replay:
            print("ERROR: Outputs between record and replay differ!")
            sys.exit(1)
    print("[10/10] OK!")
