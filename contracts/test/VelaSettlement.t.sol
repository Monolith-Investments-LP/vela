// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import "forge-std/Test.sol";
import "../src/VelaSettlement.sol";
import "@openzeppelin/contracts/token/ERC20/ERC20.sol";

contract MockERC20 is ERC20 {
    constructor() ERC20("Mock", "MCK") {
        _mint(msg.sender, 1_000_000e18);
    }
}

contract VelaSettlementTest is Test {
    VelaSettlement settlement;
    MockERC20 token;

    uint256 operatorKey = 0xA11CE;
    address operator;
    address user = address(0xBEEF);

    function setUp() public {
        operator = vm.addr(operatorKey);
        settlement = new VelaSettlement(operator);
        token = new MockERC20();

        vm.deal(user, 10 ether);
        token.transfer(user, 1000e18);
    }

    function _operatorSigFor(VelaSettlement target, address _user, address asset, uint256 amount, uint256 nonce)
        internal
        view
        returns (bytes memory)
    {
        bytes32 hash = target.withdrawHash(_user, asset, amount, nonce);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(operatorKey, hash);
        return abi.encodePacked(r, s, v);
    }

    function _operatorSig(address _user, address asset, uint256 amount, uint256 nonce)
        internal
        view
        returns (bytes memory)
    {
        return _operatorSigFor(settlement, _user, asset, amount, nonce);
    }

    function test_depositETH() public {
        vm.prank(user);
        settlement.depositETH{ value: 1 ether }();
        assertEq(settlement.getBalance(user, address(0)), 1 ether);
    }

    function test_initiateAndExecuteEmergencyExit() public {
        vm.prank(user);
        settlement.depositETH{ value: 1 ether }();

        vm.prank(user);
        settlement.initiateEmergencyExit(address(0));

        vm.warp(block.timestamp + 7 days);

        uint256 balBefore = user.balance;
        vm.prank(user);
        settlement.executeEmergencyExit(address(0));

        assertEq(settlement.getBalance(user, address(0)), 0);
        assertEq(user.balance, balBefore + 1 ether);
    }

    function test_emergencyExitRevertsBeforeTimelock() public {
        vm.prank(user);
        settlement.depositETH{ value: 1 ether }();

        vm.prank(user);
        settlement.initiateEmergencyExit(address(0));

        vm.warp(block.timestamp + 6 days);

        vm.prank(user);
        vm.expectRevert(VelaSettlement.TimelockActive.selector);
        settlement.executeEmergencyExit(address(0));
    }

    function test_withdrawWithValidSignature() public {
        vm.prank(user);
        settlement.depositETH{ value: 1 ether }();

        bytes memory sig = _operatorSig(user, address(0), 0.5 ether, 1);

        uint256 balBefore = user.balance;
        vm.prank(user);
        settlement.withdraw(address(0), 0.5 ether, 1, sig);

        assertEq(settlement.getBalance(user, address(0)), 0.5 ether);
        assertEq(user.balance, balBefore + 0.5 ether);
        assertTrue(settlement.usedWithdrawNonces(user, 1));
    }

    function test_withdrawRevertsInvalidSignature() public {
        vm.prank(user);
        settlement.depositETH{ value: 1 ether }();

        uint256 wrongKey = 0xBAD;
        bytes32 hash = settlement.withdrawHash(user, address(0), 0.5 ether, 1);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(wrongKey, hash);
        bytes memory badSig = abi.encodePacked(r, s, v);

        vm.prank(user);
        vm.expectRevert(VelaSettlement.InvalidSignature.selector);
        settlement.withdraw(address(0), 0.5 ether, 1, badSig);
    }

    function test_withdrawRevertsInsufficientBalance() public {
        vm.prank(user);
        settlement.depositETH{ value: 0.1 ether }();

        bytes memory sig = _operatorSig(user, address(0), 1 ether, 1);

        vm.prank(user);
        vm.expectRevert(VelaSettlement.InsufficientBalance.selector);
        settlement.withdraw(address(0), 1 ether, 1, sig);
    }

    // ------------------------------------------------------------------
    // Nonce-replay protection (P0 regression fixed 2026-08-31)
    // ------------------------------------------------------------------

    /// Two withdrawals of the same amount and nonce must not both succeed,
    /// even if the user has redeposited enough to cover the second call.
    function test_replayAfterRedepositIsRejected() public {
        vm.prank(user);
        settlement.depositETH{ value: 1 ether }();

        bytes memory sig = _operatorSig(user, address(0), 0.5 ether, 1);

        vm.prank(user);
        settlement.withdraw(address(0), 0.5 ether, 1, sig);

        // User redeposits enough to make the balance check pass a second time.
        vm.prank(user);
        settlement.depositETH{ value: 0.5 ether }();

        vm.prank(user);
        vm.expectRevert(VelaSettlement.NonceAlreadyUsed.selector);
        settlement.withdraw(address(0), 0.5 ether, 1, sig);
    }

    /// A signature valid on one deployment must not be accepted by a
    /// second deployment of the same contract on the same chain.
    function test_crossContractReplayIsRejected() public {
        vm.prank(user);
        settlement.depositETH{ value: 1 ether }();

        VelaSettlement other = new VelaSettlement(operator);
        vm.deal(user, user.balance + 1 ether);
        vm.prank(user);
        other.depositETH{ value: 1 ether }();

        bytes memory sigForA = _operatorSigFor(settlement, user, address(0), 0.5 ether, 42);

        vm.prank(user);
        vm.expectRevert(VelaSettlement.InvalidSignature.selector);
        other.withdraw(address(0), 0.5 ether, 42, sigForA);
    }

    /// A high-`s` signature (malleability variant) must be rejected.
    /// OZ ECDSA reverts with a specific error inside `ECDSA.recover`.
    function test_signatureMalleabilityRejected() public {
        vm.prank(user);
        settlement.depositETH{ value: 1 ether }();

        bytes32 hash = settlement.withdrawHash(user, address(0), 0.5 ether, 1);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(operatorKey, hash);

        // Flip s to its high-half counterpart. secp256k1 group order n:
        uint256 n = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;
        bytes32 highS = bytes32(n - uint256(s));
        uint8 flippedV = v == 27 ? 28 : 27;
        bytes memory malleableSig = abi.encodePacked(r, highS, flippedV);

        vm.prank(user);
        vm.expectRevert(); // ECDSA.ECDSAInvalidSignatureS
        settlement.withdraw(address(0), 0.5 ether, 1, malleableSig);
    }

    /// Fuzz: distinct nonces are independent — using nonce N does not
    /// consume nonce M, and each replay is rejected on its own.
    function testFuzz_nonceIndependence(uint96 a, uint96 b) public {
        vm.assume(a != b);
        vm.assume(a != 0 && b != 0);

        vm.deal(user, 10 ether);
        vm.prank(user);
        settlement.depositETH{ value: 4 ether }();

        bytes memory sigA = _operatorSig(user, address(0), 0.1 ether, a);
        bytes memory sigB = _operatorSig(user, address(0), 0.1 ether, b);

        vm.prank(user);
        settlement.withdraw(address(0), 0.1 ether, a, sigA);
        vm.prank(user);
        settlement.withdraw(address(0), 0.1 ether, b, sigB);

        assertTrue(settlement.usedWithdrawNonces(user, a));
        assertTrue(settlement.usedWithdrawNonces(user, b));

        vm.prank(user);
        settlement.depositETH{ value: 0.2 ether }();

        vm.prank(user);
        vm.expectRevert(VelaSettlement.NonceAlreadyUsed.selector);
        settlement.withdraw(address(0), 0.1 ether, a, sigA);
    }
}
