#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    coins, to_json_binary, wasm_execute, Addr, BankMsg, Binary, Deps, DepsMut, Empty, Env,
    MessageInfo, Order, Response, StdResult, Storage, SubMsg, Uint128,
};

use cw2::set_contract_version;
use cw20::Denom;
use cw4::{
    Member, MemberChangedHookMsg, MemberDiff, MemberListResponse, MemberResponse,
    TotalWeightResponse,
};
use cw_storage_plus::Bound;
use cw_utils::{maybe_addr, NativeBalance};
use kujira::CallbackData;

use crate::error::ContractError;
use crate::msg::{ExecuteMsg, InstantiateMsg, MigrateMsg, QueryMsg, StakedResponse};
use crate::state::{Config, ADMIN, CLAIMS, CONFIG, DAO_DAO, HOOKS, MEMBERS, STAKE, TOTAL};

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:cw4-stake";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn migrate(deps: DepsMut, _env: Env, msg: MigrateMsg) -> StdResult<Response> {
    DAO_DAO.save(deps.storage, &msg.dao_dao_addr)?;
    Ok(Response::default())
}

// Note, you can use StdResult in some functions where you do not
// make use of the custom errors
#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    mut deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    let api = deps.api;
    ADMIN.set(deps.branch(), maybe_addr(api, msg.admin)?)?;

    // min_bond is at least 1, so 0 stake -> non-membership
    let min_bond = std::cmp::max(msg.min_bond, Uint128::new(1));

    let config = Config {
        denom: msg.denom,
        tokens_per_weight: msg.tokens_per_weight,
        min_bond,
        unbonding_period: msg.unbonding_period,
    };
    CONFIG.save(deps.storage, &config)?;
    TOTAL.save(deps.storage, &0)?;

    Ok(Response::default())
}

// And declare a custom Error variant for the ones where you will want to make use of it
#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::MigrateToDaoDao { num } => {
            let config = CONFIG.load(deps.storage)?;
            let iter = MEMBERS.range(deps.storage, None, None, Order::Ascending);
            let items = iter
                .map(|r| r.map(|(addr, weight)| (addr, weight.into())))
                .take(num as usize)
                .collect::<StdResult<Vec<_>>>()?;
            // remove all members
            let mut sum = Uint128::zero();
            for (addr, weight) in &items {
                MEMBERS.remove(deps.storage, addr, env.block.height)?;
                sum -= weight;
            }
            let total = Uint128::from(TOTAL.load(deps.storage)?) - sum;
            TOTAL.save(deps.storage, &total.u128().try_into().unwrap())?;

            let msg = dao_voting_token_staked::msg::ExecuteMsg::MigrateStakes { weights: items };

            let denom = if let Denom::Native(denom) = &config.denom {
                denom.as_str()
            } else {
                unreachable!("CW20 not supported on Kujira");
            };
            let execute =
                wasm_execute(DAO_DAO.load(deps.storage)?, &msg, coins(sum.u128(), denom))?;

            Ok(Response::new()
                .add_message(execute)
                .add_attribute("action", "migrate"))
        }
        ExecuteMsg::Claim { callback } => execute_claim(deps, env, info, callback),
    }
}

pub fn must_pay_funds(balance: &NativeBalance, denom: &str) -> Result<Uint128, ContractError> {
    match balance.0.len() {
        0 => Err(ContractError::NoFunds {}),
        1 => {
            let balance = &balance.0;
            let payment = balance[0].amount;
            if balance[0].denom == denom {
                Ok(payment)
            } else {
                Err(ContractError::MissingDenom(denom.to_string()))
            }
        }
        _ => Err(ContractError::ExtraDenoms(denom.to_string())),
    }
}

fn update_membership(
    storage: &mut dyn Storage,
    sender: Addr,
    new_stake: Uint128,
    cfg: &Config,
    height: u64,
) -> StdResult<Vec<SubMsg>> {
    // update their membership weight
    let new = calc_weight(new_stake, cfg);
    let old = MEMBERS.may_load(storage, &sender)?;

    // short-circuit if no change
    if new == old {
        return Ok(vec![]);
    }
    // otherwise, record change of weight
    match new.as_ref() {
        Some(w) => MEMBERS.save(storage, &sender, w, height),
        None => MEMBERS.remove(storage, &sender, height),
    }?;

    // update total
    TOTAL.update(storage, |total| -> StdResult<_> {
        Ok(total + new.unwrap_or_default() - old.unwrap_or_default())
    })?;

    // alert the hooks
    let diff = MemberDiff::new(sender, old, new);
    HOOKS.prepare_hooks(storage, |h| {
        MemberChangedHookMsg::one(diff.clone())
            .into_cosmos_msg(h)
            .map(SubMsg::new)
    })
}

fn calc_weight(stake: Uint128, cfg: &Config) -> Option<u64> {
    if stake < cfg.min_bond {
        None
    } else {
        let w = stake.u128() / (cfg.tokens_per_weight.u128());
        Some(w as u64)
    }
}

pub fn execute_claim(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    callback: Option<CallbackData>,
) -> Result<Response, ContractError> {
    let release = CLAIMS.claim_tokens(deps.storage, &info.sender, &env.block, None)?;
    if release.is_zero() {
        return Err(ContractError::NothingToClaim {});
    }

    let config = CONFIG.load(deps.storage)?;
    let (amount_str, message) = match &config.denom {
        Denom::Native(denom) => {
            let amount_str = coin_to_string(release, denom.as_str());
            let amount = coins(release.u128(), denom);
            let msg = match callback {
                None => BankMsg::Send {
                    to_address: info.sender.to_string(),
                    amount,
                }
                .into(),
                Some(cb) => cb.to_message(&info.sender, Empty {}, amount)?,
            };
            let message = SubMsg::new(msg);
            (amount_str, message)
        }
        Denom::Cw20(_) => unreachable!("CW20 not supported on Kujira"),
    };

    Ok(Response::new()
        .add_submessage(message)
        .add_attribute("action", "claim")
        .add_attribute("tokens", amount_str)
        .add_attribute("sender", info.sender))
}

#[inline]
fn coin_to_string(amount: Uint128, denom: &str) -> String {
    format!("{} {}", amount, denom)
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Member {
            addr,
            at_height: height,
        } => to_json_binary(&query_member(deps, addr, height)?),
        QueryMsg::ListMembers { start_after, limit } => {
            to_json_binary(&list_members(deps, start_after, limit)?)
        }
        QueryMsg::TotalWeight {} => to_json_binary(&query_total_weight(deps)?),
        QueryMsg::Claims { address } => {
            to_json_binary(&CLAIMS.query_claims(deps, &deps.api.addr_validate(&address)?)?)
        }
        QueryMsg::Staked { address } => to_json_binary(&query_staked(deps, address)?),
        QueryMsg::Admin {} => to_json_binary(&ADMIN.query_admin(deps)?),
        QueryMsg::Hooks {} => to_json_binary(&HOOKS.query_hooks(deps)?),
        QueryMsg::Config {} => to_json_binary(&CONFIG.load(deps.storage)?),
    }
}

fn query_total_weight(deps: Deps) -> StdResult<TotalWeightResponse> {
    let weight = TOTAL.load(deps.storage)?;
    Ok(TotalWeightResponse { weight })
}

pub fn query_staked(deps: Deps, addr: String) -> StdResult<StakedResponse> {
    let addr = deps.api.addr_validate(&addr)?;
    let stake = STAKE.may_load(deps.storage, &addr)?.unwrap_or_default();
    let denom = CONFIG.load(deps.storage)?.denom;
    Ok(StakedResponse { stake, denom })
}

fn query_member(deps: Deps, addr: String, height: Option<u64>) -> StdResult<MemberResponse> {
    let addr = deps.api.addr_validate(&addr)?;
    let weight = match height {
        Some(h) => MEMBERS.may_load_at_height(deps.storage, &addr, h),
        None => MEMBERS.may_load(deps.storage, &addr),
    }?;
    Ok(MemberResponse { weight })
}

// settings for pagination
const MAX_LIMIT: u32 = 30;
const DEFAULT_LIMIT: u32 = 10;

fn list_members(
    deps: Deps,
    start_after: Option<String>,
    limit: Option<u32>,
) -> StdResult<MemberListResponse> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT) as usize;
    let addr = maybe_addr(deps.api, start_after)?;
    let start = addr.as_ref().map(Bound::exclusive);

    let members = MEMBERS
        .range(deps.storage, start, None, Order::Ascending)
        .take(limit)
        .map(|item| {
            item.map(|(addr, weight)| Member {
                addr: addr.into(),
                weight,
            })
        })
        .collect::<StdResult<_>>()?;

    Ok(MemberListResponse { members })
}
