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
    create_test_account,
    deploy_token_pool,
    solana,
    solana_program_deploy,
    spl_token,
    spl_token_balance,
    start_validator,
    restart_validator,
    compile_bpf_program,
    read_last_mev_log,
)


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

# creates token 0 and token account
t0_mint_keypair = create_test_account(f'{test_dir}/token0-mint.json', fund=False)
spl_token('create-token', f'{test_dir}/token0-mint.json', '--decimals', '6')
token0_mint_address = t0_mint_keypair.pubkey
print('> Token 0: ', t0_mint_keypair.pubkey)

t0_account = create_test_account(f'{test_dir}/token0-account.json', fund=False)
spl_token(
    'create-account', token0_mint_address, t0_account.keypair_path, '--output', 'json'
)
spl_token('mint', t0_mint_keypair.pubkey, '2.1', t0_account.pubkey)
print('> Minted ourselves 2.1 token 0.')


# creates token 1 and token account
t1_mint_keypair = create_test_account(f'{test_dir}/token1-mint.json', fund=False)
spl_token('create-token', f'{test_dir}/token1-mint.json', '--decimals', '6')
token1_mint_address = t1_mint_keypair.pubkey
print('> Token 1: ', t1_mint_keypair.pubkey)

t1_account = create_test_account(f'{test_dir}/token1-account.json', fund=False)
spl_token(
    'create-account', token1_mint_address, t1_account.keypair_path, '--output', 'json'
)
spl_token('mint', t1_mint_keypair.pubkey, '2.1', t1_account.pubkey)
print('> Minted ourselves 2.1 token 1.')

print('\nSetting up pool ...')

# creates pool accounts and transfer tokens
pool_t0_keypair = create_test_account(f'{test_dir}/pool-t0.json', fund=False)
pool_t1_keypair = create_test_account(f'{test_dir}/pool-t1.json', fund=False)

spl_token('create-account', token0_mint_address, pool_t0_keypair.keypair_path)
print('> Created account pool_token0: ', pool_t0_keypair.pubkey)

spl_token('create-account', token1_mint_address, pool_t1_keypair.keypair_path)
print('> Created account pool_token1: ', pool_t1_keypair.pubkey)

spl_token(
    'transfer',
    token0_mint_address,
    '1.1',
    pool_t0_keypair.pubkey,
    '--from',
    t0_account.pubkey,
)
spl_token(
    'transfer',
    token1_mint_address,
    '1.1',
    pool_t1_keypair.pubkey,
    '--from',
    t1_account.pubkey,
)


# get info to make sure transfer is working
print(
    f'> Pool owns {spl_token_balance(pool_t0_keypair.pubkey).balance_ui} of {token0_mint_address}'
)
print(
    f'> Pool owns {spl_token_balance(pool_t1_keypair.pubkey).balance_ui} of {token1_mint_address}'
)


# create pool address
token_pool = deploy_token_pool(
    token_swap_program_id, pool_t0_keypair.pubkey, pool_t1_keypair.pubkey
)
print(f'> Token Pool created with address {token_pool.token_swap_account}')

## create toml file
d_data = {
    'log_path': '/tmp/mev.log',
    'orca_program_id': token_swap_program_id,
    'orca_account': [
        {
            '_id': 'T0/T1',
            'address': token_pool.token_swap_account,
            'pool_a_account': pool_t0_keypair.pubkey,
            'pool_b_account': pool_t1_keypair.pubkey,
        }
    ],
}

with open(config_file, 'w+') as f:
    toml.dump(d_data, f)

## will stop and re-start validator with toml file
test_validator = restart_validator(test_validator, config_file=config_file)

print(f'Swapping tokens ...')

print(f'> Swapping directly')
tx_hash = token_pool.swap(
    token_a_client=t0_account.pubkey,
    token_b_client=t1_account.pubkey,
    amount=1_000,
    minimum_amount_out=0,
)

# check log is working for swaps
mev_last_line = read_last_mev_log('/tmp/mev.log')
assert mev_last_line['transaction_hash'] == tx_hash

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
tx_hash = token_pool.inner_swap(
    inner_program=inner_token_swap_program_id,
    token_a_client=t0_account.pubkey,
    token_b_client=t1_account.pubkey,
    amount=100,
    minimum_amount_out=0,
)

# check log is working for swaps
mev_last_line = read_last_mev_log('/tmp/mev.log')
assert mev_last_line['transaction_hash'] == tx_hash

test_validator.terminate()
