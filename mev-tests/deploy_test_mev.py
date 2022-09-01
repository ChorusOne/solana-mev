#!/usr/bin/env python3

# SPDX-FileCopyrightText: 2021 Chorus One AG
# SPDX-License-Identifier: GPL-3.0

"""
Set up a Orca instance on local testnet and test logging tx,
and arb opportunities. 
"""

import os
import subprocess

from uuid import uuid4

from util import (
    create_test_account,
    rpc_get_account_info,
    solana,
    solana_program_deploy,
    spl_token,
)

orca_program_id = '9W959DqEETiGZocYWCQPaJ6sBmUzgfxXfqGeTEdp3aQP'
# https://solscan.io/account/9W959DqEETiGZocYWCQPaJ6sBmUzgfxXfqGeTEdp3aQP

# replace to use ENV vars
s_dir = os.getcwd()
deploy_path = s_dir + '/mev-tests/target/deploy'

# Create a fresh directory where we store all the keys and configuration for this
# deployment.
run_id = uuid4().hex[:10]
test_dir = f'mev-tests/.keys/{run_id}'
os.makedirs(test_dir, exist_ok=True)
print(f'Keys directory: {test_dir}')

# Before we start, check our current balance. We also do this at the end,
# and then we know how much the deployment cost.
sol_balance_pre = float(solana('balance').split(' ')[0])

# If the Orca program exists, use that, otherwise upload it at a new address.
amm_info = rpc_get_account_info(orca_program_id)
if amm_info is not None:
    print('\nFound existing instance of Orca Token Swap program.')
    token_swap_program_id = orca_program_id
else:
    print('\nUploading Orca Token Swap program ...')
    # first run: 
    # solana program dump ORCA_PROGRAM_ID 'path/orca_token_swap_v2.so'
    token_swap_program_id = solana_program_deploy(
        deploy_path + '/orca_token_swap_v2.so'
    )
print(f'> Token swap program id is {token_swap_program_id}')

# Next up is the token pool, but to be able to set that up,
# we need tokens and then we need to put that in some new accounts
# that the pool will take ownership of.
t0_mint_keypair = create_test_account(f'{test_dir}/token0-mint.json', fund=False)
spl_token('create-token', f'{test_dir}/token0-mint.json', '--decimals', '6')
token0_mint_address = t0_mint_keypair.pubkey
t0_account_info_json = spl_token('create-account', token0_mint_address, '--output', 'json')
spl_token('mint', t0_mint_keypair.pubkey, '0.1')
print('> Minted ourselves 0.1 token 0.')

t1_mint_keypair = create_test_account(f'{test_dir}/token1-mint.json', fund=False)
spl_token('create-token', f'{test_dir}/token1-mint.json', '--decimals', '6')
token1_mint_address = t1_mint_keypair.pubkey
t1_account_info_json = spl_token('create-account', token1_mint_address, '--output', 'json')
spl_token('mint', t1_mint_keypair.pubkey, '0.1')
print('> Minted ourselves 0.1 token 1.')

print('\nSetting up pool ...')

# pool accounts and transfer tokens
pool_t0_keypair = create_test_account(f'{test_dir}/pool-t0.json', fund=False)
pool_t1_keypair = create_test_account(f'{test_dir}/pool-t1.json', fund=False)
spl_token('create-account', token0_mint_address, pool_t0_keypair.keypair_path)
spl_token('create-account', token1_mint_address, pool_t1_keypair.keypair_path)

spl_token('transfer', token0_mint_address, '0.1', pool_t0_keypair.pubkey)
spl_token('transfer', token1_mint_address, '0.1', pool_t1_keypair.pubkey)
