# SPDX-FileCopyrightText: 2021 Chorus One AG
# SPDX-License-Identifier: GPL-3.0

"""
Utilities that help writing tests, mainly for invoking programs.
"""

import json
import os.path
import time
import subprocess
import sys

from urllib import request
from uuid import uuid4

from typing import List, NamedTuple, Any, Optional, Dict


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


class TokenPool(NamedTuple):
    token_swap_program_id: str
    token_swap_account: str
    token_swap_a_account: str
    token_swap_b_account: str
    pool_mint_account: str
    pool_fee_account: str

    def swap(
        self,
        token_a_client: str,
        token_b_client: str,
        amount: int,
        minimum_amount_out: int,
    ) -> str:
        swap_json = json.loads(
            run(
                'cargo',
                'run',
                '--manifest-path',
                './mev-tests/helper-programs/token-swap-cli/Cargo.toml',
                '--',
                '--token-swap-program-id',
                self.token_swap_program_id,
                '--token-swap-a-account',
                self.token_swap_a_account,
                '--token-swap-b-account',
                self.token_swap_b_account,
                'swap',
                '--token-swap-account',
                self.token_swap_account,
                '--token-a-client',
                token_a_client,
                '--token-b-client',
                token_b_client,
                '--pool-mint',
                self.pool_mint_account,
                '--pool-fee',
                self.pool_fee_account,
                '--amount',
                str(amount),
                '--minimum-amount-out',
                str(minimum_amount_out),
            )
        )
        return swap_json['Swap']

    def inner_swap(
        self,
        inner_program: str,
        token_a_client: str,
        token_b_client: str,
        amount: int,
        minimum_amount_out: int,
    ) -> str:
        swap_json = json.loads(
            run(
                'cargo',
                'run',
                '--manifest-path',
                './mev-tests/helper-programs/token-swap-cli/Cargo.toml',
                '--',
                '--token-swap-program-id',
                self.token_swap_program_id,
                '--token-swap-a-account',
                self.token_swap_a_account,
                '--token-swap-b-account',
                self.token_swap_b_account,
                'inner-swap',
                '--caller-account',
                inner_program,
                '--token-swap-account',
                self.token_swap_account,
                '--token-a-client',
                token_a_client,
                '--token-b-client',
                token_b_client,
                '--pool-mint',
                self.pool_mint_account,
                '--pool-fee',
                self.pool_fee_account,
                '--amount',
                str(amount),
                '--minimum-amount-out',
                str(minimum_amount_out),
            )
        )
        return swap_json['Swap']


def deploy_token_pool(
    token_swap_program_id: str, token_swap_a_account: str, token_swap_b_account: str
) -> TokenPool:
    init_json = json.loads(
        run(
            'cargo',
            'run',
            '--manifest-path',
            './mev-tests/helper-programs/token-swap-cli/Cargo.toml',
            '--',
            '--token-swap-program-id',
            token_swap_program_id,
            '--token-swap-a-account',
            token_swap_a_account,
            '--token-swap-b-account',
            token_swap_b_account,
            'init',
        )
    )['SwapInit']
    return TokenPool(
        token_swap_program_id=token_swap_program_id,
        token_swap_account=init_json['address'],
        token_swap_a_account=token_swap_a_account,
        token_swap_b_account=token_swap_b_account,
        pool_mint_account=init_json['pool_mint'],
        pool_fee_account=init_json['pool_fee'],
    )


def start_validator(config_path: Optional[str] = None) -> subprocess.Popen[bytes]:
    """
    Start the validator with an optional .toml config file, pipe its stdout to /dev/null.
    """
    cmds = ['solana-test-validator']
    if config_path is not None:
        cmds += ['--mev-config-path', config_path]
    test_validator = subprocess.Popen(
        cmds,
        stdout=subprocess.DEVNULL,
        # Somehow, CI only works if `shell=True`, so this argument is left here on
        # purpose. This doesn't capture arguments (or paths, need to investigate),
        # For now, we'll keep it commented.
        # shell=True,
    )
    wait_validator()
    return test_validator


def restart_validator(
    test_validator: subprocess.Popen[bytes], config_file: Optional[str] = None
) -> subprocess.Popen[bytes]:
    """
    Stops a running validator and re-start keeping the ledger
    """
    test_validator.terminate()
    sleep_seconds = 2
    if not test_validator.poll():
        time.sleep(sleep_seconds)

    ## restart validator with toml file
    new_validator = start_validator(config_path=config_file)
    wait_validator()
    return new_validator


def wait_validator() -> None:
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


def compile_bpf_program(cargo_manifest: str) -> None:
    run(
        'cargo',
        'build-bpf',
        '--manifest-path',
        cargo_manifest,
    )


def read_last_mev_log(log_path: str):
    with open(log_path, 'r') as f:
        for line in f:
            last_line = line
    return json.loads(last_line)
