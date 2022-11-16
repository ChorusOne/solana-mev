#!/usr/bin/env python3

# SPDX-FileCopyrightText: 2021 Chorus One AG
# SPDX-License-Identifier: GPL-3.0

"""
Set up a Orca instance on local testnet and test logging tx,
and arb opportunities. 
"""

import sys, os
from typing import Optional
import toml

from uuid import uuid4

from util import (
    TokenPool,
    TestAccount,
    create_test_account,
    deploy_token_pool,
    solana,
    solana_program_deploy,
    spl_token,
    start_validator,
    restart_validator,
    compile_bpf_program,
    read_mev_log,
)


def create_account_and_mint_tokens(
    test_dir: str, amount: str, mint_address: str
) -> TestAccount:
    id = uuid4().hex[:10]
    token_account = create_test_account(
        f'{test_dir}/token-{id}-account.json', fund=False
    )
    spl_token(
        'create-account',
        mint_address,
        token_account.keypair_path,
        '--output',
        'json',
    )
    spl_token('mint', mint_address, amount, token_account.pubkey)
    return token_account


def create_token_pool_with_liquidity(
    test_dir: str,
    pool_id: str,
    token_swap_program_id: str,
    token_a_mint: TestAccount,
    token_b_mint: TestAccount,
    token_a_liquidity: str,
    token_b_liquidity: str,
) -> TokenPool:
    t_a_account = create_test_account(
        f'{test_dir}/token-{pool_id}-token-0-account.json', fund=False
    )
    spl_token(
        'create-account',
        token_a_mint.pubkey,
        t_a_account.keypair_path,
        '--output',
        'json',
    )
    spl_token('mint', token_a_mint.pubkey, token_a_liquidity, t_a_account.pubkey)
    print(f'> Minted ourselves {token_a_liquidity} of token 0 from {pool_id}.')

    t_b_account = create_test_account(f'{test_dir}/token1-account.json', fund=False)
    spl_token(
        'create-account',
        token_b_mint.pubkey,
        t_b_account.keypair_path,
        '--output',
        'json',
    )
    spl_token('mint', token_b_mint.pubkey, token_b_liquidity, t_b_account.pubkey)
    print(f'> Minted ourselves {token_b_liquidity} of token 1 from {pool_id}.')

    token_pool = deploy_token_pool(
        token_swap_program_id,
        t_a_account.pubkey,
        t_b_account.pubkey,
        token_a_mint.pubkey,
        token_b_mint.pubkey,
    )

    return token_pool


# replace to use ENV vars
s_dir = os.getcwd()
deploy_path = s_dir + '/mev-tests/target/deploy'

# validator config filename
config_file = 'mev-tests/mev_config.toml'

# Start the validator, pipe its stdout to /dev/null.
test_validator = start_validator()


# Create a fresh directory where we store all the keys and configuration for this
# deployment.
run_id = uuid4().hex[:10]
test_dir = f'mev-tests/.keys/{run_id}'
os.makedirs(test_dir, exist_ok=True)
print(f'Keys directory: {test_dir}')

# Before we start, check our current balance. We also do this at the end,
# and then we know how much the deployment cost.
sol_balance_pre = float(solana('balance').split(' ')[0])

# since the validator will be started in every new test,
# it doesn't need to check if Orca program exist.
# We can just deploy it anyway.
print('\nUploading Orca Token Swap program ...')

# first run:
# solana program dump ORCA_PROGRAM_ID 'path/orca_token_swap_v2.so'
token_swap_program_id = solana_program_deploy(deploy_path + '/orca_token_swap_v2.so')
print(f'> Token swap program id is {token_swap_program_id}')


miner_authority_key = create_test_account(f'{test_dir}/miner_authority.json', fund=True)
token_mint_keypairs = []
pool_tokens = []
# Create tokens
for i in range(3):
    token_mint = create_test_account(f'{test_dir}/token-{i}-mint.json', fund=False)
    token_mint_keypairs.append(token_mint)
    spl_token(
        'create-token',
        f'{test_dir}/token-{i}-mint.json',
        '--decimals',
        '9',
    )
    token_account = create_test_account(
        f'{test_dir}/miner-token-account-{i}.json', fund=False
    )
    pool_tokens.append(token_account.pubkey)
    spl_token(
        'create-account',
        token_mint.pubkey,
        token_account.keypair_path,
        '--owner',
        miner_authority_key.pubkey,
        '--output',
        'json',
    )

token_pool_p0 = create_token_pool_with_liquidity(
    test_dir,
    'P0',
    token_swap_program_id,
    token_mint_keypairs[0],
    token_mint_keypairs[1],
    '32500.951164566',
    '1030.701091486',
)
print(f'> Token Pool created with address {token_pool_p0.token_swap_account}')

token_pool_p1 = create_token_pool_with_liquidity(
    test_dir,
    'P1',
    token_swap_program_id,
    token_mint_keypairs[0],
    token_mint_keypairs[2],
    '6761.724934325',
    '15.245225568',
)
print(f'> Token Pool created with address {token_pool_p1.token_swap_account}')

token_pool_p2 = create_token_pool_with_liquidity(
    test_dir,
    'P2',
    token_swap_program_id,
    token_mint_keypairs[2],
    token_mint_keypairs[1],
    '0.000453975',
    '0.006517227',
)
print(f'> Token Pool created with address {token_pool_p2.token_swap_account}')

## create toml file
d_data = {
    'log_path': '/tmp/mev.log',
    'orca_program_id': token_swap_program_id,
    'user_authority_path': miner_authority_key.keypair_path,
    'orca_account': [
        {
            '_id': 'P0: Token0, Token1',
            'address': token_pool_p0.token_swap_account,
            'pool_a_account': token_pool_p0.token_swap_a_account,
            'pool_b_account': token_pool_p0.token_swap_b_account,
            'pool_mint': token_pool_p0.pool_mint_account,
            'pool_fee': token_pool_p0.pool_fee_account,
            'source': pool_tokens[0],
            'destination': pool_tokens[1],
        },
        {
            '_id': 'P1: Token0, Token2',
            'address': token_pool_p1.token_swap_account,
            'pool_a_account': token_pool_p1.token_swap_a_account,
            'pool_b_account': token_pool_p1.token_swap_b_account,
            'pool_mint': token_pool_p1.pool_mint_account,
            'pool_fee': token_pool_p1.pool_fee_account,
            'source': pool_tokens[0],
            'destination': pool_tokens[2],
        },
        {
            '_id': 'Token2, Token1',
            'address': token_pool_p2.token_swap_account,
            'pool_a_account': token_pool_p2.token_swap_a_account,
            'pool_b_account': token_pool_p2.token_swap_b_account,
            'pool_mint': token_pool_p2.pool_mint_account,
            'pool_fee': token_pool_p2.pool_fee_account,
            'source': pool_tokens[2],
            'destination': pool_tokens[1],
        },
    ],
    'mev_path': [
        {
            'name': 'P0->P1->P2',
            'path': [
                {'pool': token_pool_p0.token_swap_account, 'direction': 'BtoA'},
                {'pool': token_pool_p1.token_swap_account, 'direction': 'AtoB'},
                {'pool': token_pool_p2.token_swap_account, 'direction': 'AtoB'},
            ],
        }
    ],
}
# Mint T1 Token to ourselves so we can extract MEV:

spl_token('mint', token_mint_keypairs[1].pubkey, '1.0', pool_tokens[1])
initial_balance = float(spl_token('balance', '--address', pool_tokens[1]))
print(
    f'  Minted ourselves {initial_balance} token {token_mint_keypairs[1].pubkey}\
into {pool_tokens[1]} so we can extract opportunities'
)

with open(config_file, 'w+') as f:
    toml.dump(d_data, f)

## will stop and re-start validator with toml file
test_validator = restart_validator(test_validator, config_file=config_file)

print(f'\nSwapping tokens ...')

print(f'> Minting ourselves some tokens')
t0_account = create_account_and_mint_tokens(
    test_dir, '2.1', token_pool_p0.token_mint_a_account
)
t1_account = create_account_and_mint_tokens(
    test_dir, '2.1', token_pool_p0.token_mint_b_account
)

print(f'> Swapping directly')
tx_hash = token_pool_p0.swap(
    token_a_client=t0_account.pubkey,
    token_b_client=t1_account.pubkey,
    amount=1_000,
    minimum_amount_out=0,
)

# check log is working for swaps
mev_logs = read_mev_log('/tmp/mev.log')
assert mev_logs[len(mev_logs) - 3]['transaction_hash'] == tx_hash

assert mev_logs[len(mev_logs) - 2] == {
    'event': 'opportunity',
    'data': [
        {
            'opportunity': {
                'name': 'P0->P1->P2',
                'path': [
                    {
                        'pool': token_pool_p0.token_swap_account,
                        'direction': 'BtoA',
                    },
                    {
                        'pool': token_pool_p1.token_swap_account,
                        'direction': 'AtoB',
                    },
                    {
                        'pool': token_pool_p2.token_swap_account,
                        'direction': 'AtoB',
                    },
                ],
            },
            'input_output_pairs': [
                {'token_in': 36868, 'token_out': 1159084},
                {'token_in': 1159084, 'token_out': 2605},
                {'token_in': 2605, 'token_out': 37084},
            ],
        }
    ],
}

assert mev_logs[len(mev_logs) - 1]['data']['is_successful'] == True
assert mev_logs[len(mev_logs) - 1]['data']['possible_profit'] == 216

post_balance = float(spl_token('balance', '--address', pool_tokens[1]))
assert int(post_balance * 1e9) - int(initial_balance * 1e9) == 216


print('> Compiling the BPF program to swap with an inner program')
compile_bpf_program(
    cargo_manifest=s_dir + '/mev-tests/helper-programs/inner-swap-program/Cargo.toml'
)
print('> Uploading inner token swap program ...')

inner_swap_deploy_path = (
    s_dir + '/mev-tests/helper-programs/target/deploy/inner_swap.so'
)
inner_token_swap_program_id = solana_program_deploy(inner_swap_deploy_path)
print(f'> Inner token swap program id is {inner_token_swap_program_id}')

print('> Swapping with an inner program')
tx_hash = token_pool_p0.inner_swap(
    inner_program=inner_token_swap_program_id,
    token_a_client=t0_account.pubkey,
    token_b_client=t1_account.pubkey,
    amount=100,
    minimum_amount_out=0,
)

# check log is working for swaps
mev_logs = read_mev_log('/tmp/mev.log')
assert mev_logs[len(mev_logs) - 1]['transaction_hash'] == tx_hash

test_validator.terminate()
