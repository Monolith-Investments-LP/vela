// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import "forge-std/Test.sol";
import "../src/VelaSettlement.sol";
import "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import "@openzeppelin/contracts/access/Ownable.sol";
import "@openzeppelin/contracts/utils/Pausable.sol";

/// @dev Standard ERC20 used by the fund-conservation invariant.
contract MockERC20 is ERC20 {
    constructor() ERC20("Mock", "MCK") {
        _mint(msg.sender, 1_000_000e18);
    }
}

/// @dev Fee-on-transfer ERC20 that keeps 1% of every `transferFrom`.
contract FeeOnTransferToken is ERC20 {
    constructor() ERC20("FoT", "FOT") {
        _mint(msg.sender, 1_000_000e18);
    }

    function _update(address from, address to, uint256 amount) internal override {
        if (from == address(0) || to == address(0)) {
            super._update(from, to, amount);
            return;
        }
        uint256 fee = amount / 100;
        super._update(from, address(0xdead), fee);
        super._update(from, to, amount - fee);
    }
}

/// @dev ERC20 that always reverts on `transferFrom`.
contract RevertingToken is ERC20 {
    constructor() ERC20("Rev", "REV") {
        _mint(msg.sender, 1_000_000e18);
    }

    function transferFrom(address, address, uint256) public pure override returns (bool) {
        revert("noop");
    }
}

contract VelaSettlementHardeningTest is Test {
    VelaSettlement settlement;
    MockERC20 token;
    FeeOnTransferToken fot;
    RevertingToken rev;

    uint256 operatorKey = 0xA11CE;
    address operator;
    address user = address(0xBEEF);
    address guardian;

    function setUp() public {
        operator = vm.addr(operatorKey);
        settlement = new VelaSettlement(operator);
        guardian = address(this); // deployer defaults to guardian
        token = new MockERC20();
        fot = new FeeOnTransferToken();
        rev = new RevertingToken();

        vm.deal(user, 100 ether);
        token.transfer(user, 1000e18);
        fot.transfer(user, 1000e18);
        rev.transfer(user, 1000e18);
    }

    // ---------------------------------------------------------------
    // Pause / guardian model
    // ---------------------------------------------------------------

    function test_guardianCanPause_ownerCanUnpause() public {
        settlement.pause();
        assertTrue(settlement.paused());

        vm.prank(user);
        vm.expectRevert(Pausable.EnforcedPause.selector);
        settlement.depositETH{ value: 1 ether }();

        settlement.unpause();
        assertFalse(settlement.paused());

        vm.prank(user);
        settlement.depositETH{ value: 1 ether }();
        assertEq(settlement.getBalance(user, address(0)), 1 ether);
    }

    function test_guardianCannotUnpause() public {
        settlement.pause();
        // Rotate guardian to a fresh account and confirm they can't unpause.
        address newGuardian = address(0xCAFE);
        settlement.setGuardian(newGuardian);
        vm.prank(newGuardian);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, newGuardian));
        settlement.unpause();
    }

    function test_emergencyExitStillWorksWhilePaused() public {
        vm.prank(user);
        settlement.depositETH{ value: 1 ether }();
        vm.prank(user);
        settlement.initiateEmergencyExit(address(0));
        settlement.pause();

        vm.warp(block.timestamp + 7 days);

        uint256 balBefore = user.balance;
        vm.prank(user);
        settlement.executeEmergencyExit(address(0));
        assertEq(user.balance, balBefore + 1 ether);
        // Pause did NOT trap funds — that's the invariant we care about.
    }

    // ---------------------------------------------------------------
    // Operator rotation timelock
    // ---------------------------------------------------------------

    function test_operatorRotationRespectsTimelock() public {
        address newOperator = address(0xC0FFEE);
        settlement.proposeOperator(newOperator);
        assertEq(settlement.pendingOperator(), newOperator);

        vm.expectRevert(VelaSettlement.OperatorTimelockActive.selector);
        settlement.acceptProposedOperator();

        vm.warp(block.timestamp + 48 hours);
        settlement.acceptProposedOperator();
        assertEq(settlement.operator(), newOperator);
        assertEq(settlement.pendingOperator(), address(0));
    }

    function test_proposeOperatorRejectsZeroAddress() public {
        vm.expectRevert(VelaSettlement.ZeroAddress.selector);
        settlement.proposeOperator(address(0));
    }

    // ---------------------------------------------------------------
    // ERC20 edge cases
    // ---------------------------------------------------------------

    /// Fee-on-transfer tokens over-credit users if the contract naively
    /// trusts the requested amount. VelaSettlement uses
    /// `safeTransferFrom` and credits `amount` — meaning FoT tokens
    /// leave the contract short by the fee. We surface that here so
    /// operators know to allowlist non-FoT assets only.
    function test_depositTokenOverCreditsFeeOnTransfer() public {
        vm.startPrank(user);
        fot.approve(address(settlement), 100e18);
        settlement.depositToken(address(fot), 100e18);
        vm.stopPrank();
        // Contract credited full amount, but actually received 99e18.
        assertEq(settlement.getBalance(user, address(fot)), 100e18);
        assertEq(fot.balanceOf(address(settlement)), 99e18);
        // Documented risk: the last withdrawal would fail
        // safeTransfer. Guarded operationally by not listing FoT tokens.
    }

    /// A reverting ERC20 must not corrupt state — the whole tx reverts.
    function test_depositTokenRevertingErc20BubblesUp() public {
        vm.startPrank(user);
        rev.approve(address(settlement), 100e18);
        vm.expectRevert();
        settlement.depositToken(address(rev), 100e18);
        vm.stopPrank();
        assertEq(settlement.getBalance(user, address(rev)), 0);
    }
}

// ---------------------------------------------------------------
// Invariant: fund conservation
// ---------------------------------------------------------------

/// @dev Wrapper handler that constrains random calls to a set of safe
/// signatures. Foundry's invariant harness only calls functions on
/// contracts registered via `targetContract`, so we route everything
/// through here.
contract SettlementHandler is Test {
    VelaSettlement public settlement;
    address[] public users;
    address public token;

    constructor(VelaSettlement _settlement, address _token, address[] memory _users) {
        settlement = _settlement;
        token = _token;
        users = _users;
    }

    function depositETH(uint256 idx, uint256 amount) external {
        idx = bound(idx, 0, users.length - 1);
        amount = bound(amount, 1, 1 ether);
        vm.deal(users[idx], amount);
        vm.prank(users[idx]);
        settlement.depositETH{ value: amount }();
    }

    function depositToken(uint256 idx, uint256 amount) external {
        idx = bound(idx, 0, users.length - 1);
        amount = bound(amount, 1, 1e18);
        // Mint into the user's balance from the initial supply held here.
        IERC20(token).transfer(users[idx], amount);
        vm.startPrank(users[idx]);
        IERC20(token).approve(address(settlement), amount);
        settlement.depositToken(token, amount);
        vm.stopPrank();
    }
}

contract VelaSettlementInvariantTest is Test {
    VelaSettlement settlement;
    MockERC20 token;
    SettlementHandler handler;
    address[] users;

    function setUp() public {
        settlement = new VelaSettlement(address(this));
        token = new MockERC20();

        users = new address[](3);
        users[0] = address(0x1111);
        users[1] = address(0x2222);
        users[2] = address(0x3333);

        // Pre-fund the handler so it can front-load deposits.
        token.transfer(address(this), 500_000e18);
        handler = new SettlementHandler(settlement, address(token), users);
        token.transfer(address(handler), 500_000e18);

        targetContract(address(handler));
    }

    /// Sum of every user's ETH balance in the ledger equals the raw ETH
    /// balance the contract holds. Guards against a rounding or over-
    /// credit bug in the deposit path.
    function invariant_ethConservation() public view {
        uint256 sum;
        for (uint256 i = 0; i < users.length; i++) {
            sum += settlement.getBalance(users[i], address(0));
        }
        assertEq(sum, address(settlement).balance);
    }

    /// Same invariant for the ERC20 side.
    function invariant_erc20Conservation() public view {
        uint256 sum;
        for (uint256 i = 0; i < users.length; i++) {
            sum += settlement.getBalance(users[i], address(token));
        }
        assertEq(sum, token.balanceOf(address(settlement)));
    }
}
