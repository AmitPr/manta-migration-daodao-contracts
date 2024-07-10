use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::{Addr, Uint128};

use cw20::Denom;
pub use cw_controllers::ClaimsResponse;
use cw_utils::Duration;
use kujira::CallbackData;

use crate::state::Config;

#[cw_serde]
pub struct InstantiateMsg {
    /// denom of the token to stake
    pub denom: Denom,
    pub tokens_per_weight: Uint128,
    pub min_bond: Uint128,
    pub unbonding_period: Duration,

    // admin can only add/remove hooks, not change other parameters
    pub admin: Option<String>,
}

#[cw_serde]
pub struct MigrateMsg {
    pub dao_dao_addr: Addr,
}

#[cw_serde]
pub enum ExecuteMsg {
    /// Migrates a batch of user stakes to DAO DAO.
    MigrateToDaoDao { num: u64, num_claims: u64 },
}

#[cw_serde]
#[derive(QueryResponses)]
pub enum QueryMsg {
    /// Claims shows the tokens in process of unbonding for this address
    #[returns(cw_controllers::ClaimsResponse)]
    Claims { address: String },
    // Show the number of tokens currently staked by this address.
    #[returns(StakedResponse)]
    Staked { address: String },

    #[returns(cw_controllers::AdminResponse)]
    Admin {},
    #[returns(cw4::TotalWeightResponse)]
    TotalWeight {},
    #[returns(cw4::MemberListResponse)]
    ListMembers {
        start_after: Option<String>,
        limit: Option<u32>,
    },
    #[returns(cw4::MemberResponse)]
    Member {
        addr: String,
        at_height: Option<u64>,
    },
    /// Shows all registered hooks.
    #[returns(cw_controllers::HooksResponse)]
    Hooks {},
    /// Returns the config
    #[returns(Config)]
    Config {},
}

#[cw_serde]
pub struct StakedResponse {
    pub stake: Uint128,
    pub denom: Denom,
}
