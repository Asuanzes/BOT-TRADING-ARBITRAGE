// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {PredictionMarket} from "../src/PredictionMarket.sol";

contract PredictionMarketTest is Test {
    PredictionMarket internal pm;
    address internal owner    = address(0xABCD);
    address internal operator = address(0xBEEF);
    address internal alice    = address(0xA11CE);
    address internal bob      = address(0xB0B);

    function setUp() public {
        vm.prank(owner);
        pm = new PredictionMarket(operator);
    }

    // -------- Faucet --------

    function test_claimInitialBalance_givesThreeThousand() public {
        vm.prank(alice);
        pm.claimInitialBalance();
        assertEq(pm.balanceUsdMillis(alice), 3_000_000);
        assertTrue(pm.claimedInitial(alice));
    }

    function test_claimInitialBalance_revertsOnDoubleClaim() public {
        vm.startPrank(alice);
        pm.claimInitialBalance();
        vm.expectRevert(PredictionMarket.AlreadyClaimed.selector);
        pm.claimInitialBalance();
        vm.stopPrank();
    }

    // -------- Round lifecycle --------

    function test_openRound_onlyOperator() public {
        vm.prank(alice);
        vm.expectRevert(PredictionMarket.NotOperator.selector);
        pm.openRound(int192(60_000 * 1e18));
    }

    function test_openRound_setsStateAndEmits() public {
        vm.prank(operator);
        uint256 roundId = pm.openRound(int192(60_000 * 1e18));
        assertEq(roundId, 1);
        assertEq(pm.currentRoundId(), 1);
        (uint64 startTs, uint64 endTs, int192 startPrice,, PredictionMarket.RoundState state) = pm.rounds(1);
        assertEq(uint256(state), uint256(PredictionMarket.RoundState.Open));
        assertEq(startTs, uint64(block.timestamp));
        assertEq(endTs, uint64(block.timestamp) + 5 minutes);
        assertEq(startPrice, int192(60_000 * 1e18));
    }

    function test_openRound_revertsIfPreviousNotResolved() public {
        vm.startPrank(operator);
        pm.openRound(int192(60_000 * 1e18));
        vm.expectRevert(PredictionMarket.PreviousRoundNotResolved.selector);
        pm.openRound(int192(60_100 * 1e18));
        vm.stopPrank();
    }

    function test_closeRound_revertsBeforeEndTs() public {
        vm.prank(operator);
        pm.openRound(int192(60_000 * 1e18));
        vm.prank(operator);
        vm.expectRevert(PredictionMarket.RoundStillOpen.selector);
        pm.closeRound(int192(60_100 * 1e18));
    }

    function test_closeRound_resolvesAfterEndTs() public {
        vm.prank(operator);
        pm.openRound(int192(60_000 * 1e18));
        vm.warp(block.timestamp + 5 minutes);
        vm.prank(operator);
        pm.closeRound(int192(60_100 * 1e18));
        (,,, int192 endPrice, PredictionMarket.RoundState state) = pm.rounds(1);
        assertEq(uint256(state), uint256(PredictionMarket.RoundState.Resolved));
        assertEq(endPrice, int192(60_100 * 1e18));
    }

    // -------- Bet validations --------

    function _seedAndOpen(address user) internal {
        vm.prank(user);
        pm.claimInitialBalance();
        vm.prank(operator);
        pm.openRound(int192(60_000 * 1e18));
    }

    function test_bet_succeedsAndDebitsBalance() public {
        _seedAndOpen(alice);
        vm.prank(alice);
        pm.bet(PredictionMarket.Direction.Up, 50_000, 520); // $50 on UP at $0.52
        assertEq(pm.balanceUsdMillis(alice), 3_000_000 - 50_000);
        PredictionMarket.Position memory p = pm.getPosition(1, alice);
        assertTrue(p.exists);
        assertEq(uint256(p.dir), uint256(PredictionMarket.Direction.Up));
        assertEq(p.amountUsdMillis, 50_000);
        assertEq(p.entryTokenPriceMillis, 520);
    }

    function test_bet_revertsAboveMax() public {
        _seedAndOpen(alice);
        vm.prank(alice);
        vm.expectRevert(PredictionMarket.AmountExceedsMax.selector);
        pm.bet(PredictionMarket.Direction.Up, 100_001, 520);
    }

    function test_bet_revertsZeroAmount() public {
        _seedAndOpen(alice);
        vm.prank(alice);
        vm.expectRevert(PredictionMarket.AmountZero.selector);
        pm.bet(PredictionMarket.Direction.Up, 0, 520);
    }

    function test_bet_revertsAboveBalance() public {
        // No faucet claim ⇒ balance is 0; any positive amount within MAX_BET
        // must trip AmountExceedsBalance.
        vm.prank(operator);
        pm.openRound(int192(60_000 * 1e18));
        vm.prank(alice);
        vm.expectRevert(PredictionMarket.AmountExceedsBalance.selector);
        pm.bet(PredictionMarket.Direction.Up, 50_000, 520);
    }

    function test_bet_revertsTooLate() public {
        _seedAndOpen(alice);
        vm.warp(block.timestamp + 5 minutes - 59); // 59 s left
        vm.prank(alice);
        vm.expectRevert(PredictionMarket.TooLateToBet.selector);
        pm.bet(PredictionMarket.Direction.Up, 50_000, 520);
    }

    function test_bet_revertsInvalidTokenPrice() public {
        _seedAndOpen(alice);
        vm.prank(alice);
        vm.expectRevert(PredictionMarket.InvalidTokenPrice.selector);
        pm.bet(PredictionMarket.Direction.Up, 50_000, 0);
        vm.prank(alice);
        vm.expectRevert(PredictionMarket.InvalidTokenPrice.selector);
        pm.bet(PredictionMarket.Direction.Up, 50_000, 1000);
    }

    function test_bet_revertsIfPositionExists() public {
        _seedAndOpen(alice);
        vm.startPrank(alice);
        pm.bet(PredictionMarket.Direction.Up, 50_000, 520);
        vm.expectRevert(PredictionMarket.PositionExists.selector);
        pm.bet(PredictionMarket.Direction.Up, 10_000, 520);
        vm.stopPrank();
    }

    // -------- Sell math --------

    function test_sell_payoutMathProfit() public {
        _seedAndOpen(alice);
        vm.startPrank(alice);
        // Buy UP @ $0.40 with $50 ⇒ 125 tokens. Sell @ $0.60 ⇒ 125 × 0.60 = $75.
        pm.bet(PredictionMarket.Direction.Up, 50_000, 400);
        uint256 payout = pm.sell(600);
        vm.stopPrank();
        assertEq(payout, 75_000); // $75 in millis
        assertEq(pm.balanceUsdMillis(alice), 3_000_000 - 50_000 + 75_000);
    }

    function test_sell_payoutMathLoss() public {
        _seedAndOpen(alice);
        vm.startPrank(alice);
        pm.bet(PredictionMarket.Direction.Up, 50_000, 600); // buy expensive
        uint256 payout = pm.sell(300); // dump cheap
        vm.stopPrank();
        // 50 × 300 / 600 = 25
        assertEq(payout, 25_000);
        assertEq(pm.balanceUsdMillis(alice), 3_000_000 - 50_000 + 25_000);
    }

    function test_sell_clearsPositionAndAllowsReentry() public {
        _seedAndOpen(alice);
        vm.startPrank(alice);
        pm.bet(PredictionMarket.Direction.Up, 50_000, 400);
        pm.sell(600);
        // Reentry on the same round must succeed (>60 s remaining was the only on-chain rule).
        pm.bet(PredictionMarket.Direction.Down, 30_000, 480);
        vm.stopPrank();
        PredictionMarket.Position memory p = pm.getPosition(1, alice);
        assertTrue(p.exists);
        assertEq(uint256(p.dir), uint256(PredictionMarket.Direction.Down));
        assertEq(p.amountUsdMillis, 30_000);
    }

    function test_sell_revertsNoPosition() public {
        _seedAndOpen(alice);
        vm.prank(alice);
        vm.expectRevert(PredictionMarket.NoActivePosition.selector);
        pm.sell(500);
    }

    // -------- Claim --------

    function test_claim_winnerUpStrictlyAbove() public {
        _seedAndOpen(alice);
        vm.prank(alice);
        pm.bet(PredictionMarket.Direction.Up, 50_000, 400); // 125 tokens at full $1.00 = $125
        vm.warp(block.timestamp + 5 minutes);
        vm.prank(operator);
        pm.closeRound(int192(60_100 * 1e18)); // BTC went up

        vm.prank(alice);
        uint256 payout = pm.claim(1);
        assertEq(payout, 125_000); // $125
        assertEq(pm.balanceUsdMillis(alice), 3_000_000 - 50_000 + 125_000);
    }

    function test_claim_loserGetsZero() public {
        _seedAndOpen(alice);
        vm.prank(alice);
        pm.bet(PredictionMarket.Direction.Up, 50_000, 400);
        vm.warp(block.timestamp + 5 minutes);
        vm.prank(operator);
        pm.closeRound(int192(59_900 * 1e18)); // BTC went down

        vm.prank(alice);
        uint256 payout = pm.claim(1);
        assertEq(payout, 0);
        // Balance stays at 2_950_000 (the $50 was already debited at bet time).
        assertEq(pm.balanceUsdMillis(alice), 2_950_000);
    }

    function test_claim_tieFavorsUp() public {
        _seedAndOpen(alice);
        // Alice bets UP, Bob bets DOWN, end == start.
        vm.prank(alice);
        pm.bet(PredictionMarket.Direction.Up, 50_000, 400);
        vm.prank(bob);
        pm.claimInitialBalance();
        vm.prank(bob);
        pm.bet(PredictionMarket.Direction.Down, 50_000, 600);

        vm.warp(block.timestamp + 5 minutes);
        vm.prank(operator);
        pm.closeRound(int192(60_000 * 1e18)); // tie

        vm.prank(alice);
        uint256 alicePayout = pm.claim(1);
        vm.prank(bob);
        uint256 bobPayout = pm.claim(1);

        assertEq(alicePayout, 125_000); // 50 / 0.40 = $125
        assertEq(bobPayout, 0);
    }

    function test_claim_revertsBeforeResolved() public {
        _seedAndOpen(alice);
        vm.prank(alice);
        pm.bet(PredictionMarket.Direction.Up, 50_000, 400);
        vm.prank(alice);
        vm.expectRevert(PredictionMarket.RoundNotResolved.selector);
        pm.claim(1);
    }

    function test_claim_revertsNoPosition() public {
        vm.prank(operator);
        pm.openRound(int192(60_000 * 1e18));
        vm.warp(block.timestamp + 5 minutes);
        vm.prank(operator);
        pm.closeRound(int192(60_100 * 1e18));

        vm.prank(alice);
        vm.expectRevert(PredictionMarket.NoPositionToClaim.selector);
        pm.claim(1);
    }

    function test_claim_revertsDoubleClaim() public {
        _seedAndOpen(alice);
        vm.prank(alice);
        pm.bet(PredictionMarket.Direction.Up, 50_000, 400);
        vm.warp(block.timestamp + 5 minutes);
        vm.prank(operator);
        pm.closeRound(int192(60_100 * 1e18));
        vm.prank(alice);
        pm.claim(1);
        vm.prank(alice);
        vm.expectRevert(PredictionMarket.NoPositionToClaim.selector);
        pm.claim(1);
    }
}
