#!/usr/bin/env python3

# SPDX-FileCopyrightText: 2021 Chorus One AG
# SPDX-License-Identifier: GPL-3.0

"""
Start a test validator and wait for it to be available, then print its PID.
This script is used to start the test validator on CI.
"""

import subprocess
import sys
import time
from tracemalloc import start

from typing import Optional


def start_validator():
    # Start the validator, pipe its stdout to /dev/null.
    test_validator = subprocess.Popen(
        ['solana-test-validator'],
        stdout=subprocess.DEVNULL,
        # Somehow, CI only works if `shell=True`, so this argument is left here on
        # purpose.
        shell=True,
    )

    return test_validator


def start_validator_config(config_path='config.toml'):
    # Start the validator, pipe its stdout to /dev/null.
    test_validator = subprocess.Popen(
        ['solana-test-validator', '--mev-config-path', config_path],
        stdout=subprocess.DEVNULL,
        # Somehow, CI only works if `shell=True`, so this argument is left here on
        # purpose.
        shell=True,
    )

    return test_validator


def restart_validator(test_validator, config_file=None):
    ## stop validator
    test_validator.terminate()
    sleep_seconds = 2
    if not test_validator.poll():
        time.sleep(sleep_seconds)

    ## restart validator with toml file
    if config_file is not None:
        new_validator = start_validator_config(config_file)
    else:
        new_validator = start_validator()
    return new_validator


# Wait up to 60 seconds for the validator to be running and processing blocks. We
# check this by running "solana block-height", and observing at least one
# increase. If that is the case, the RPC is available, and the validator must be
# producing blocks. Previously we only checked "solana cluster-version", but
# this can return a response before the validator is ready to accept
# transactions.
def check_validator_running(test_validator):
    last_observed_block_height: Optional[int] = None

    for _ in range(60):
        result = subprocess.run(
            ['solana', '--url', 'http://127.0.0.1:8899', 'block-height'],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        if result.returncode == 0:
            current_block_height = int(result.stdout)
            if (
                last_observed_block_height is not None
                and current_block_height > last_observed_block_height
            ):
                break
            last_observed_block_height = current_block_height

        sleep_seconds = 1
        time.sleep(sleep_seconds)

    is_rpc_online = last_observed_block_height is not None

    if is_rpc_online and test_validator.poll() is None:
        # The RPC is online, and the process is still running.
        print(test_validator.pid)

    elif is_rpc_online:
        print(
            'RPC is online, but the process is gone ... was a validator already running?',
            file=sys.stderr,
        )
        sys.exit(2)

    else:
        print(
            'Test validator is still not responding, something is wrong.',
            file=sys.stderr,
        )
        sys.exit(3)
