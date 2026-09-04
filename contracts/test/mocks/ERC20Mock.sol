// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/**
 * @dev A plain ERC-20 to stand in for USDT/USDC in tests: no permit, no hooks, the
 * smallest surface that still exercises the treasury's SafeERC20 path the way a real
 * stablecoin on Fuji would.
 */
contract ERC20Mock is ERC20 {
  constructor() ERC20("Mock USD", "MUSD") {}

  /// Test-only faucet: the real tokens on Fuji are faucet/minted by their owners;
  /// tests need balances without buying any.
  function mint(address to, uint256 amount) external {
    _mint(to, amount);
  }
}
