#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    coins, to_json_binary, wasm_execute, Addr, Binary, Deps, DepsMut, Env, MessageInfo, Order,
    Response, StdResult, Uint128,
};

use cw2::set_contract_version;
use cw20::Denom;
use cw4::{Member, MemberListResponse, MemberResponse, TotalWeightResponse};
use cw_controllers::Claim;
use cw_storage_plus::{Bound, Map};
use cw_utils::{maybe_addr, NativeBalance};

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
    _info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::MigrateToDaoDao { num, num_claims } => {
            let config = CONFIG.load(deps.storage)?;
            let iter = STAKE.range(deps.storage, None, None, Order::Ascending);
            let weights = iter.take(num as usize).collect::<StdResult<Vec<_>>>()?;
            // remove all members
            let mut sum = Uint128::zero();
            let mut weight_sum = 0u64;
            for (addr, weight) in &weights {
                STAKE.remove(deps.storage, addr);
                let vote_weight = MEMBERS.may_load(deps.storage, addr)?.unwrap_or_default();
                MEMBERS.remove(deps.storage, addr, env.block.height)?;
                sum += weight;
                weight_sum += vote_weight;
            }
            let total = TOTAL.load(deps.storage)? - weight_sum;
            TOTAL.save(deps.storage, &total)?;

            // Also migrate claims
            let claims_map: Map<Addr, Vec<Claim>> = Map::new("claims");
            let iter = claims_map.range(deps.storage, None, None, Order::Ascending);
            let claims = iter
                .take(num_claims as usize)
                .collect::<StdResult<Vec<_>>>()?;

            for (addr, claims) in &claims {
                claims.iter().for_each(|c| sum += c.amount);
                claims_map.remove(deps.storage, addr.clone());
            }

            let msg = dao_voting_token_staked::msg::ExecuteMsg::MigrateStakes { weights, claims };

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
