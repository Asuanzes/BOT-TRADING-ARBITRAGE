// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title  PredictionMarket — virtual UP/DOWN BTC market resolved by Chainlink Streams.
/// @notice Replicates Polymarket's 5-minute BTC up/down markets with virtual USD balances.
///         No ETH or ERC-20 custody: every user has a virtual `balanceUsdMillis` that grows
///         when they win and shrinks when they lose. Round lifecycle (open/close prices)
///         is driven off-chain today (operator address) and will move behind a
///         StreamsLookup + Automation upkeep in a follow-up step.
contract PredictionMarket {
    enum Direction { Up, Down }
    enum RoundState { None, Open, Resolved }

    struct Round {
        uint64 startTs;
        uint64 endTs;
        int192 startPrice;   // BTC/USD scaled by 1e18 (Chainlink Streams v3 convention)
        int192 endPrice;
        RoundState state;
    }

    /// @dev One active position per user per round. Sell deletes it; bet creates it.
    struct Position {
        Direction dir;
        uint256 amountUsdMillis;
        uint256 entryTokenPriceMillis; // 1000 ≡ $1.00. Polymarket binary tokens trade in (0, 1).
        bool exists;
    }

    // ----- Constants (USD-millis: 1_000 ≡ $1.00, lets us avoid decimals) -----
    uint256 public constant INITIAL_BALANCE_USD_MILLIS = 3_000_000;   // $3,000.000
    uint256 public constant MAX_BET_USD_MILLIS         = 100_000;     // $100.000
    uint256 public constant MIN_REMAINING_SECS_FOR_BET = 60;
    uint256 public constant TOKEN_PRICE_DENOMINATOR    = 1_000;       // 1_000 ≡ $1.00
    uint64  public constant ROUND_DURATION             = 5 minutes;

    // ----- Storage -----
    address public owner;
    address public operator;
    uint256 public currentRoundId;

    mapping(uint256 => Round) public rounds;
    mapping(uint256 => mapping(address => Position)) private _positions;
    mapping(address => uint256) public balanceUsdMillis;
    mapping(address => bool) public claimedInitial;

    // ----- Events -----
    event InitialBalanceClaimed(address indexed user, uint256 amountUsdMillis);
    event OperatorUpdated(address indexed previousOperator, address indexed newOperator);
    event RoundOpened(uint256 indexed roundId, uint64 startTs, uint64 endTs, int192 startPrice);
    event RoundResolved(uint256 indexed roundId, int192 endPrice, bool upWins);
    event BetPlaced(
        uint256 indexed roundId,
        address indexed user,
        Direction dir,
        uint256 amountUsdMillis,
        uint256 entryTokenPriceMillis
    );
    event BetSold(
        uint256 indexed roundId,
        address indexed user,
        uint256 sellTokenPriceMillis,
        uint256 payoutUsdMillis,
        int256  pnlUsdMillis
    );
    event PayoutClaimed(uint256 indexed roundId, address indexed user, bool won, uint256 payoutUsdMillis);

    // ----- Errors -----
    error NotOwner();
    error NotOperator();
    error AlreadyClaimed();
    error RoundNotOpen();
    error RoundNotResolved();
    error RoundStillOpen();
    error PreviousRoundNotResolved();
    error PositionExists();
    error NoActivePosition();
    error AmountExceedsMax();
    error AmountExceedsBalance();
    error AmountZero();
    error TooLateToBet();
    error InvalidTokenPrice();
    error NoPositionToClaim();

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    modifier onlyOperator() {
        if (msg.sender != operator) revert NotOperator();
        _;
    }

    constructor(address _operator) {
        owner = msg.sender;
        operator = _operator;
        emit OperatorUpdated(address(0), _operator);
    }

    // -------------------- Admin --------------------

    function setOperator(address newOperator) external onlyOwner {
        emit OperatorUpdated(operator, newOperator);
        operator = newOperator;
    }

    // -------------------- Faucet --------------------

    /// @notice One-shot virtual balance grant. The user's PnL evolves freely from
    /// here on; there is no cap and no recharge.
    function claimInitialBalance() external {
        if (claimedInitial[msg.sender]) revert AlreadyClaimed();
        claimedInitial[msg.sender] = true;
        balanceUsdMillis[msg.sender] = INITIAL_BALANCE_USD_MILLIS;
        emit InitialBalanceClaimed(msg.sender, INITIAL_BALANCE_USD_MILLIS);
    }

    // -------------------- Round lifecycle --------------------

    /// @notice Opens a new round with the BTC price observed off-chain via Chainlink
    /// Streams. Reverts if the previous round is not yet Resolved (only one round
    /// active at a time keeps the contract simple and matches Polymarket's cadence).
    function openRound(int192 startPrice) external onlyOperator returns (uint256 roundId) {
        if (currentRoundId != 0 && rounds[currentRoundId].state != RoundState.Resolved) {
            revert PreviousRoundNotResolved();
        }
        roundId = currentRoundId + 1;
        currentRoundId = roundId;
        Round storage r = rounds[roundId];
        r.startTs    = uint64(block.timestamp);
        r.endTs      = uint64(block.timestamp) + ROUND_DURATION;
        r.startPrice = startPrice;
        r.state      = RoundState.Open;
        emit RoundOpened(roundId, r.startTs, r.endTs, startPrice);
    }

    /// @notice Resolves the active round with the closing BTC price from Streams.
    /// `upWins = endPrice >= startPrice` — the tie case favors UP, matching the
    /// Polymarket BTC up/down market description.
    function closeRound(int192 endPrice) external onlyOperator {
        Round storage r = rounds[currentRoundId];
        if (r.state != RoundState.Open) revert RoundNotOpen();
        if (block.timestamp < r.endTs) revert RoundStillOpen();
        r.endPrice = endPrice;
        r.state    = RoundState.Resolved;
        emit RoundResolved(currentRoundId, endPrice, endPrice >= r.startPrice);
    }

    // -------------------- Bet / Sell / Claim --------------------

    /// @notice Open a position on the active round. The bot passes the live
    /// Polymarket CLOB buy price for the chosen side (UP or DOWN) as
    /// `entryTokenPriceMillis` (1_000 ≡ $1.00). The contract trusts this price
    /// and uses it to compute payouts on `sell` and `claim`.
    function bet(Direction dir, uint256 amountUsdMillis, uint256 entryTokenPriceMillis) external {
        Round storage r = rounds[currentRoundId];
        if (r.state != RoundState.Open) revert RoundNotOpen();
        if (uint256(r.endTs) < block.timestamp + MIN_REMAINING_SECS_FOR_BET) revert TooLateToBet();
        if (amountUsdMillis == 0) revert AmountZero();
        if (amountUsdMillis > MAX_BET_USD_MILLIS) revert AmountExceedsMax();
        if (amountUsdMillis > balanceUsdMillis[msg.sender]) revert AmountExceedsBalance();
        if (entryTokenPriceMillis == 0 || entryTokenPriceMillis >= TOKEN_PRICE_DENOMINATOR) {
            revert InvalidTokenPrice();
        }
        if (_positions[currentRoundId][msg.sender].exists) revert PositionExists();

        balanceUsdMillis[msg.sender] -= amountUsdMillis;
        _positions[currentRoundId][msg.sender] = Position({
            dir: dir,
            amountUsdMillis: amountUsdMillis,
            entryTokenPriceMillis: entryTokenPriceMillis,
            exists: true
        });
        emit BetPlaced(currentRoundId, msg.sender, dir, amountUsdMillis, entryTokenPriceMillis);
    }

    /// @notice Sell the active position at the CLOB price reported by the bot.
    /// Frees the slot so the user can `bet` again in the same round (the bot
    /// applies its 65%-profit / momentum rules off-chain; this contract only
    /// enforces the >60 s rule via `bet`'s `TooLateToBet` check).
    function sell(uint256 sellTokenPriceMillis) external returns (uint256 payout) {
        Round storage r = rounds[currentRoundId];
        if (r.state != RoundState.Open) revert RoundNotOpen();
        Position memory p = _positions[currentRoundId][msg.sender];
        if (!p.exists) revert NoActivePosition();
        if (sellTokenPriceMillis == 0 || sellTokenPriceMillis >= TOKEN_PRICE_DENOMINATOR) {
            revert InvalidTokenPrice();
        }

        // payout = amount × sellPrice / entryPrice (Polymarket-style: tokens × sellPrice).
        payout = (p.amountUsdMillis * sellTokenPriceMillis) / p.entryTokenPriceMillis;
        delete _positions[currentRoundId][msg.sender];
        balanceUsdMillis[msg.sender] += payout;

        emit BetSold(
            currentRoundId,
            msg.sender,
            sellTokenPriceMillis,
            payout,
            int256(payout) - int256(p.amountUsdMillis)
        );
    }

    /// @notice Claim payout for an unsold position once the round is resolved.
    /// Winner payout = amount × 1.00 / entryPrice (winning token is worth $1).
    /// Loser payout = 0 (the amount was already debited at `bet`).
    function claim(uint256 roundId) external returns (uint256 payout) {
        Round storage r = rounds[roundId];
        if (r.state != RoundState.Resolved) revert RoundNotResolved();
        Position memory p = _positions[roundId][msg.sender];
        if (!p.exists) revert NoPositionToClaim();
        delete _positions[roundId][msg.sender];

        bool upWins = r.endPrice >= r.startPrice;
        bool won    = (p.dir == Direction.Up && upWins) || (p.dir == Direction.Down && !upWins);
        if (won) {
            payout = (p.amountUsdMillis * TOKEN_PRICE_DENOMINATOR) / p.entryTokenPriceMillis;
            balanceUsdMillis[msg.sender] += payout;
        }
        emit PayoutClaimed(roundId, msg.sender, won, payout);
    }

    // -------------------- Views --------------------

    function getPosition(uint256 roundId, address user) external view returns (Position memory) {
        return _positions[roundId][user];
    }
}
