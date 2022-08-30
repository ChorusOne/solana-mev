# SPDX-FileCopyrightText: 2021 Chorus One AG
# SPDX-License-Identifier: GPL-3.0

"""
Utilities that help writing tests, mainly for invoking programs.
"""

import json
import os.path
import subprocess
import sys

from urllib import request
from uuid import uuid4

from typing import List, NamedTuple, Any, Optional, Callable, Dict, Tuple

MAX_VALIDATION_COMMISSION_PERCENTAGE = 5


class TestAccount(NamedTuple):
    pubkey: str
    keypair_path: str

    def __repr__(self) -> str:
        return self.pubkey


def run(*args: str) -> str:
    """
    Run a program, ensure it exits with code 0, return its stdout.
    """
    try:
        result = subprocess.run(args, check=True, capture_output=True, encoding='utf-8')

    except subprocess.CalledProcessError as err:
        # If a test fails, it is helpful to print stdout and stderr here, but
        # we don't print them by default because some calls are expected to
        # fail, and we don't want to pollute the output in that case, because
        # a log full of errors makes it difficult to locate the actual error in
        # the noise.
        if '--verbose' in sys.argv:
            print('Command failed:', ' '.join(args))
            print('Stdout:', err.stdout)
            print('Stderr:', err.stderr)
        raise

    return result.stdout


def get_network() -> str:
    network = os.getenv('NETWORK')
    if network is None:
        return 'http://127.0.0.1:8899'
    else:
        return network


def solana(*args: str) -> str:
    """
    Run 'solana' against network.
    """
    return run('solana', '--url', get_network(), '--commitment', 'confirmed', *args)


def spl_token(*args: str) -> str:
    """
    Run 'spl_token' against network.
    """
    return run('spl-token', '--url', get_network(), *args)


class SplTokenBalance(NamedTuple):
    # The raw amount is the amount in the smallest denomination of the token
    # (i.e. the number of Lamports for wrapped SOL), the UI amount is a float
    # of `amount_raw` divided by `10^num_decimals`.
    balance_raw: int
    balance_ui: float


def spl_token_balance(address: str) -> SplTokenBalance:
    """
    Return the balance of an SPL token account.
    """
    result = run(
        'spl-token',
        '--url',
        get_network(),
        'balance',
        '--address',
        address,
        '--output',
        'json',
    )
    data: Dict[str, Any] = json.loads(result)
    amount_raw = int(data['amount'])
    amount_ui: float = data['uiAmount']
    return SplTokenBalance(amount_raw, amount_ui)


def solana_program_deploy(fname: str) -> str:
    """
    Deploy a .so file, return its program id.
    """
    assert os.path.isfile(fname)
    result = solana('program', 'deploy', '--output', 'json', fname)
    program_id: str = json.loads(result)['programId']
    return program_id


class SolanaProgramInfo(NamedTuple):
    program_id: str
    owner: str
    program_data_address: str
    upgrade_authority: str
    last_deploy_slot: int
    data_len: int


def solana_program_show(program_id: str) -> SolanaProgramInfo:
    """
    Return information about a program.,
    """
    result = solana('program', 'show', '--output', 'json', program_id)
    data: Dict[str, Any] = json.loads(result)
    return SolanaProgramInfo(
        program_id=data['programId'],
        owner=data['owner'],
        program_data_address=data['programdataAddress'],
        upgrade_authority=data['authority'],
        last_deploy_slot=data['lastDeploySlot'],
        data_len=data['dataLen'],
    )


def create_test_account(keypair_fname: str, *, fund: bool = True) -> TestAccount:
    """
    Generate a key pair, fund the account with 1 SOL, and return its public key.
    """
    run(
        'solana-keygen',
        'new',
        '--no-bip39-passphrase',
        '--force',
        '--silent',
        '--outfile',
        keypair_fname,
    )
    pubkey = run('solana-keygen', 'pubkey', keypair_fname).strip()
    if fund:
        solana('transfer', '--allow-unfunded-recipient', pubkey, '1.0')
    return TestAccount(pubkey, keypair_fname)


def create_spl_token_account(owner_keypair_fname: str, minter: str) -> str:
    """
    Creates an spl token for the given minter
    spl_token command returns 'Creating account <address>
             Signature: <tx-signature>'
    This function returns <address>
    """
    return (
        spl_token('create-account', minter, '--owner', owner_keypair_fname)
        .split('\n')[0]
        .split(' ')[2]
    )


def create_test_accounts(*, num_accounts: int) -> List[TestAccount]:
    result = []

    for i in range(num_accounts):
        fname = f'test-key-{i + 1}.json'
        test_account = create_test_account(fname)
        result.append(test_account)

    return result


def wait_for_slots(slots: int) -> None:
    import time

    """
    Blocks waiting until `slots` slots have passed.
    """
    slots_beginning = int(solana('get-slot'))
    while True:
        # Wait 1 second to poll next slot height (around 2 slots)
        time.sleep(1)
        elapsed_slots = int(solana('get-slot')) - slots_beginning
        if elapsed_slots >= slots:
            break


def solana_rpc(method: str, params: List[Any]) -> Any:
    """
    Make a Solana RPC call.

    This function is very sloppy, doesn't do proper error handling, and is not
    suitable for serious use, but for tests or checking things on devnet it's
    useful.
    """
    body = {
        'jsonrpc': '2.0',
        'id': str(uuid4()),
        'method': method,
        'params': params,
    }
    req = request.Request(
        get_network(),
        method='POST',
        data=json.dumps(body).encode('utf-8'),
        headers={
            'Content-Type': 'application/json',
        },
    )
    response = request.urlopen(req)
    return json.load(response)


def rpc_get_account_info(address: str) -> Optional[Dict[str, Any]]:
    """
    Call getAccountInfo, see https://docs.solana.com/developing/clients/jsonrpc-api#getaccountinfo.
    """
    result: Dict[str, Any] = solana_rpc(
        method='getAccountInfo',
        params=[address, {'encoding': 'jsonParsed'}],
    )
    # The value is either an object with decoded account info, or None, if the
    # account does not exist.
    account_info: Optional[Dict[str, Any]] = result['result']['value']
    return account_info