//! 内置教练：不依赖环境变量、不依赖外部 workflow 的在线自适应。
//!
//! `CG_TUNE_*` 拨盘是为本地 A/B 设计的，但内网 workflow **不能**设置环境变量，
//! 也不会为我们跑对照实验（`docs/WORKFLOW_REQUEST.md` v7 请求七已把那个实验撤回，
//! 改成请 workflow 回执每场的胜负与分项得分）。于是"哪一档更好"必须在进程内部回答：
//! 每一轮从局势里读出**可归因**的证据，证据够格之后才移动一个开关，并且随时可以
//! 因为反向证据移回来。
//!
//! 教练只碰三个**已经存在**的开关，别的什么都不碰（D1 墙体优先、`shared_wall_duty`、
//! `config::TOWER_BUILD_ORDER` 的造塔顺序、三塔上限、不覆盖现有塔、金币储备、collect→sell→buy
//! 循环、夜间召回、黄昏预定位全部原样保留）：
//!
//! | 开关 | 承诺默认 | 收紧（保守） | 放松（激进） |
//! |---|---|---|---|
//! | `station_pressure` (P2-2) | 开 | 我方基地挨揍而对方没挨 → 立刻收火 | 连着两夜我方基地毫发无损 → 重新开火 |
//! | `gap_funding` (P1-1) | 关 | 连着两夜零损失 → 认为固定排序够用 | 一夜我方基地挨揍 → 缺口估计被证实，按缺口排序 |
//! | `harass` (P2-3) | `Rhythm` | 连着两次召唤令没换来对方任何损失 → `Off`（有 4 夜试用期，到点重测一张） | 连着两夜令确实咬动过对方 → `Rich` |
//!
//! 三条都是**因果**归因，不是拟合：证据来自协议里直接可见的量（双方基地与围墙
//! 血量、我方当日召唤令计数、今晚的火力缺口估计），每一夜结算一次，跨半场按 1/2
//! 衰减继承。半场重开时 `BotState` 会被整个清空，教练跟着走（见 `state.rs`）。
//!
//! ## 归因的两个坑
//!
//! * **召唤令是隔夜生效的**（任务书 4.6：令给"对方下个夜晚"加机器人）：白天下的
//!   单，效果就落在当晚；夜里下的单，要再等一夜。两个桶分开记（`orders_from_day` /
//!   `orders_from_night`），夜里下的单在结算时原样滚到下一夜，绝不能算进当夜——
//!   当夜对方掉的每一滴血都还是上一夜那批机器人的账。
//! * **我方炮塔本来就会打对方建筑**（`station_focus` / 剩余火力），那也会让对方掉
//!   血。若那一夜我方朝对方建筑开过火（`night.rs` 的 `enemyFire`），这一夜就不能
//!   用来评价召唤令，顺延一夜。
//!
//! ## 持久化
//!
//! 证据在**半场结束**写盘（`coregeek_learn.json`，`CG_LEARN_PATH` 可改路径），
//! 下一个进程启动时读回——这样判决机即使一场一个进程，跨场学习也能积累。写盘是
//! 尽力而为：任何 IO 错误都被忽略，读不回来就是全新教练。**只有真实进程会读盘**
//! （`install()` 由 `main.rs` 调用），测试里教练是纯内存的，避免用例之间互相污染。

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::log;
use crate::model::{Turn, UnitKind, DAY_ROUNDS};

/// 连续两夜毫发无损 → 解除对外火力的抑制。
const QUIET_TO_REARM: i64 = 2;
/// 连续两次令没咬动对方 → 降到 `Off`。
const STERILE_TO_DOWNGRADE: i64 = 2;
/// 连续两次令咬动了对方 → 升到 `Rich`。
const FRUITFUL_TO_UPGRADE: i64 = 2;
/// 连续两夜零损失 → 认为固定资金排序够用。
const QUIET_TO_RELAX_GAP: i64 = 2;
/// `Off` 档的试用期：停买这么多夜之后强制重测一张。
///
/// `Off` 是个**死胡同**——不买单就永远不会有"令见效"的证据，档位会永久卡死。而
/// "令买不动对方"这个结论本身是有时效的：半场换边、换对手、对方围墙被啃塌之后，
/// 同一个门槛可能就划算了。4 夜让一个 10 天的半场里能重测两轮，又不会把刚得出的
/// 结论立刻推翻。
const OFF_PROBATION_NIGHTS: i64 = 4;
/// 证据跨半场继承时的衰减除数。
const CARRY_DECAY: i64 = 2;

/// 召唤令档位。`Rhythm` 与当前 commit 的门槛逐字一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Harass {
    /// 不买：最近的令没换来对方任何损失。
    #[default]
    Off,
    /// 承诺行为：富门槛 500 金 + 三塔，或 300 金 + 敌方墙 ≤ 3，库存 ≤ 2。
    Rhythm,
    /// 加码：库存 ≤ 3、薄墙门槛放宽到 5 面墙。只有令确实咬动过对方才升到这里。
    Rich,
}

impl Harass {
    pub fn as_str(self) -> &'static str {
        match self {
            Harass::Off => "Off",
            Harass::Rhythm => "Rhythm",
            Harass::Rich => "Rich",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "Off" => Some(Harass::Off),
            "Rhythm" => Some(Harass::Rhythm),
            "Rich" => Some(Harass::Rich),
            _ => None,
        }
    }

    /// 手里最多压几张令。
    pub fn stock_cap(self) -> i64 {
        match self {
            Harass::Off => 0,
            Harass::Rhythm => 2,
            Harass::Rich => 3,
        }
    }

    /// 薄墙门槛：敌方墙数 ≤ 这个值，300 金档才开。
    pub fn thin_wall_limit(self) -> usize {
        match self {
            Harass::Off => 0,
            Harass::Rhythm => 3,
            Harass::Rich => 5,
        }
    }
}

/// 教练能移动的三个开关。字段全是 `pub`，测试与 `CG_TUNE_*` 都能直接构造。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// P2-2 敌基地持续压制。
    pub station_pressure: bool,
    /// P1-1 按火力缺口重排资金（只在缺口为正时有意义）。
    pub gap_funding: bool,
    /// P2-3 召唤令档位。
    pub harass: Harass,
}

impl Policy {
    /// 与当前 commit 的承诺行为逐位一致——没有任何环境变量、没有任何证据时的默认档。
    pub fn committed() -> Self {
        Self {
            station_pressure: true,
            gap_funding: false,
            harass: Harass::Rhythm,
        }
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self::committed()
    }
}

/// 血量快照：一夜之间的损失从这里算出来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Snapshot {
    our_station: i64,
    their_station: i64,
    our_wall: i64,
    their_wall: i64,
}

impl Snapshot {
    fn of(turn: &Turn) -> Self {
        Self {
            our_station: station_hp(&turn.ours),
            their_station: station_hp(&turn.enemy),
            our_wall: wall_hp(&turn.ours),
            their_wall: wall_hp(&turn.enemy),
        }
    }
}

fn station_hp(units: &[crate::model::Unit]) -> i64 {
    units
        .iter()
        .find(|unit| unit.kind == UnitKind::Station)
        .map(|unit| unit.health.max(0))
        .unwrap_or(0)
}

fn wall_hp(units: &[crate::model::Unit]) -> i64 {
    units
        .iter()
        .filter(|unit| unit.kind == UnitKind::Wall && unit.health > 0)
        .map(|unit| unit.health)
        .sum()
}

/// 一夜（或半场里剩下的那半夜）的收支账本，也是打到日志里的形状。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NightLedger {
    pub our_station_lost: i64,
    pub their_station_lost: i64,
    pub our_wall_lost: i64,
    pub their_wall_lost: i64,
    /// 这一夜里用掉的召唤令张数。
    pub orders_used: i64,
    /// 我方炮塔这一夜朝对方建筑开过火（用来把"对方掉血"从召唤令的功劳里剔除）。
    pub our_fire_on_enemy: bool,
}

impl NightLedger {
    pub fn our_loss(&self) -> i64 {
        self.our_station_lost + self.our_wall_lost
    }

    pub fn their_loss(&self) -> i64 {
        self.their_station_lost + self.their_wall_lost
    }

    fn idle(&self) -> bool {
        self.our_loss() == 0 && self.their_loss() == 0 && self.orders_used == 0
    }
}

/// 由 `main.rs` 在开始服务前调用一次：读回上一场学到的证据，并允许本场写盘。
///
/// 只有真实进程会调它——测试里 `BotState::default()` 拿到的是纯内存教练，用例之间
/// 不会通过磁盘互相污染。
///
/// `CG_LEARN=0` 关掉整个教练（档位冻结在承诺默认值），`CG_LEARN_PATH` 改文件位置。
pub fn install() {
    let enabled = std::env::var("CG_LEARN")
        .map(|value| value != "0" && !value.eq_ignore_ascii_case("off"))
        .unwrap_or(true);
    let coach = if enabled {
        restore(true)
    } else {
        Coach::frozen()
    };
    crate::state::BotState::install_coach(coach);
}

/// 从磁盘读回一个教练（仅当 `persist`）。任何错误都退化成全新教练。
fn restore(persist: bool) -> Coach {
    let mut coach = Coach::default();
    coach.persist = persist;
    if !persist {
        return coach;
    }
    let Some(path) = learn_path() else {
        return coach;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return coach;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return coach;
    };
    Coach::from_json(&value, persist)
}

/// 学习文件位置：`CG_LEARN_PATH` > 进程工作目录下的 `coregeek_learn.json`。
fn learn_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CG_LEARN_PATH") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    Some(PathBuf::from("coregeek_learn.json"))
}

/// 计数器快照：日志（`coach_night` 的 `evidence`）与用例共用同一份口径。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// 连续"只有我方基地在掉血"的夜数。
    pub bleed_nights: i64,
    /// 连续"我方零损失"的夜数。
    pub quiet_nights: i64,
    /// 连续"令花了但对方毫无损失"的夜数。
    pub sterile_nights: i64,
    /// 连续"令花了且对方掉了血"的夜数。
    pub fruitful_nights: i64,
    /// 连续"我方基地挨打"的夜数（火力缺口被证实）。
    pub gap_breach_nights: i64,
    /// 连续"零损失"的夜数（固定排序够用）。
    pub gap_quiet_nights: i64,
    /// 已下单、效果落在**本次结算**这一夜的令（白天下的单）。
    pub orders_from_day: i64,
    /// 已下单、效果落在**下一夜**的令（夜里下的单，结算时原样顺延）。
    pub orders_from_night: i64,
}

impl Counters {
    /// 已下单但还没轮到结算的令总数。
    pub fn pending_orders(&self) -> i64 {
        self.orders_from_day + self.orders_from_night
    }
}

/// 局势驱动的自适应教练。
#[derive(Debug, Clone)]
pub struct Coach {
    pub policy: Policy,
    /// 教练是否工作（`CG_LEARN=0` 时为假：只记录证据，不移动开关）。
    enabled: bool,
    persist: bool,
    /// 当前这一夜正在累积的账。
    ledger: NightLedger,
    /// 上一夜的账（结算后保留，供日志与测试查看）。
    pub last_night: NightLedger,
    prev: Option<Snapshot>,
    last_round: i64,
    last_in_day: Option<i64>,
    /// 今夜开始时的火力缺口估计。
    night_gap: i64,
    /// 白天下单的令：效果算在"下个夜晚"，也就是本次结算正要算的这一夜。
    orders_from_day: i64,
    /// 夜里下单的令：效果算在再下一夜，本次结算还不算。
    orders_from_night: i64,
    /// 当日召唤令计数的上次读数（`state.summon_orders_today`）。
    orders_seen_today: i64,
    /// 连续"只有我方基地在掉血"的夜数。
    bleed_nights: i64,
    /// 连续"我方零损失"的夜数。
    quiet_nights: i64,
    /// 连续"令花了但对方毫无损失"的夜数。
    sterile_nights: i64,
    /// 连续"令花了且对方掉了血"的夜数。
    fruitful_nights: i64,
    /// 连续"我方基地挨打"的夜数（火力缺口被证实）。
    gap_breach_nights: i64,
    /// 连续"零损失"的夜数（固定排序够用）。
    gap_quiet_nights: i64,
    /// `harass == Off` 已经持续了几夜（试用期计时）。
    off_nights: i64,
    /// 本进程已经走完的半场数。
    pub halves: i64,
}

impl Default for Coach {
    fn default() -> Self {
        Self {
            policy: Policy::committed(),
            enabled: true,
            persist: false,
            ledger: NightLedger::default(),
            last_night: NightLedger::default(),
            prev: None,
            last_round: 0,
            last_in_day: None,
            night_gap: 0,
            orders_from_day: 0,
            orders_from_night: 0,
            orders_seen_today: 0,
            bleed_nights: 0,
            quiet_nights: 0,
            sterile_nights: 0,
            fruitful_nights: 0,
            gap_breach_nights: 0,
            gap_quiet_nights: 0,
            off_nights: 0,
            halves: 0,
        }
    }
}

impl Coach {
    /// 关掉自适应的教练（`CG_LEARN=0`）：档位冻结在承诺默认值。
    pub fn frozen() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    pub fn policy(&self) -> Policy {
        self.policy
    }

    /// 启动时打进 `coach_ready` 的载荷。
    ///
    /// 之所以做成"从 `&self` 出载荷"而不是在 `main.rs` 里现拼 `json!`：`BotState`
    /// 是 `Mutex` 保护的全局量，`locked()` 拿的是 `MutexGuard`，在同一句里取两次
    /// 就是自我死锁（std 的 `Mutex` 不可重入），而这个函数一次只要求一个借用。
    pub fn ready_json(&self) -> Value {
        json!({
            "policy": policy_json(self.policy),
            "halvesLearned": self.halves,
        })
    }

    /// 每回合在 `BotState::observe` 末尾调用一次。
    ///
    /// `orders_today` 是 `state.summon_orders_today`（当日已用令数），教练只读它
    /// 的增量，不反过来改它。
    pub fn observe(&mut self, turn: &Turn, orders_today: i32) {
        let continuous = self.last_round > 0 && turn.round_no == self.last_round + 1;
        let previous_in_day = self.last_in_day;
        self.last_round = turn.round_no;
        self.last_in_day = Some(turn.in_day_round);

        let now = Snapshot::of(turn);
        if continuous {
            if let Some(previous) = self.prev {
                // 只有"上一回合比我方更满"的差才算损失；新落成的围墙/升级回满血
                // 会让血量上升，那一边取 0。
                self.ledger.our_station_lost += (previous.our_station - now.our_station).max(0);
                self.ledger.their_station_lost += (previous.their_station - now.their_station).max(0);
                self.ledger.our_wall_lost += (previous.our_wall - now.our_wall).max(0);
                self.ledger.their_wall_lost += (previous.their_wall - now.their_wall).max(0);
            }
        }
        self.prev = Some(now);
        self.orders_seen_today = self.absorb_orders(orders_today, turn.is_day);

        // 今夜的火力缺口：夜里挨打之后才知道这个估计准不准。
        if turn.in_day_round == DAY_ROUNDS {
            self.night_gap = crate::brain::combat::firepower_gap(turn);
        }

        // 即时抑制：本夜我方基地在掉血、对方基地没掉 → 对方的火力落在我方基地上，
        // 而我方炮塔还在"有余力"时去点对方建筑。这一刻就收火，不等夜晚结算：
        // 让基地多活一夜比多打对方建筑几十点伤害值钱得多。
        if self.enabled
            && !turn.is_day
            && self.policy.station_pressure
            && self.ledger.our_station_lost > 0
            && self.ledger.their_station_lost == 0
        {
            self.set_station_pressure(false, "our_station_bleeding", turn);
        }

        // 夜晚 → 白天：结算这一夜。
        let night_ended = continuous
            && turn.is_day
            && previous_in_day.map(|round| round >= DAY_ROUNDS).unwrap_or(false);
        if night_ended {
            self.settle(turn);
        }
    }

    /// 记录"我方炮塔这一夜朝对方建筑开过火"（`night.rs` 的 `enemyFire`）。
    /// 这一夜不能用来评价召唤令的功劳，结算顺延。
    pub fn note_enemy_fire(&mut self) {
        self.ledger.our_fire_on_enemy = true;
    }

    /// 当日召唤令计数增长 = 有人下单（白天第 12 步，或夜里的备用职责）。
    ///
    /// 计数在跨天时归零（`state.rs` 的日翻转），所以读数变小只说明换了一天，
    /// 不是"撤单"。
    ///
    /// 下单的**时点**决定了效果落在哪一夜：任务书 4.6 说令给"对方**下个**夜晚"
    /// 加机器人，所以白天下的单算当晚（本次结算正要算的这一夜），夜里下的单要
    /// 再等一夜。
    fn absorb_orders(&mut self, orders_today: i32, is_day: bool) -> i64 {
        let orders = orders_today.max(0) as i64;
        if orders > self.orders_seen_today {
            let delta = orders - self.orders_seen_today;
            self.ledger.orders_used += delta;
            if is_day {
                self.orders_from_day += delta;
            } else {
                self.orders_from_night += delta;
            }
        }
        orders
    }

    /// 一夜结束后归因、移动开关。
    fn settle(&mut self, turn: &Turn) {
        let night = std::mem::take(&mut self.ledger);

        // A. 基地交换：谁在挨打。
        if night.our_station_lost > 0 && night.their_station_lost == 0 {
            self.bleed_nights += 1;
            self.quiet_nights = 0;
        } else if night.our_station_lost == 0 {
            self.quiet_nights += 1;
            self.bleed_nights = (self.bleed_nights - 1).max(0);
        }
        if self.enabled && !self.policy.station_pressure && self.quiet_nights >= QUIET_TO_REARM {
            self.set_station_pressure(true, "two_quiet_nights", turn);
        }

        // B. 召唤令 ROI：令是给"对方下个夜晚"加的机器人（任务书 4.6）。这一夜要
        //    结的是白天下的那批单（它们的"下个夜晚"就是刚过去的这一夜）；夜里下的
        //    单效果在下一夜，原样滚过去。若我方炮火这一夜也打过对方建筑
        //    （`night.rs` 的 `enemyFire`），对方的血是谁打掉的说不清——白天那批的
        //    样本只能作废，夜里那批本来就还没到期。
        let carry = std::mem::replace(&mut self.orders_from_night, 0);
        let paid = std::mem::replace(&mut self.orders_from_day, carry);
        if paid > 0 && !night.our_fire_on_enemy {
            if night.their_loss() > 0 {
                self.fruitful_nights += 1;
                self.sterile_nights = 0;
            } else {
                self.sterile_nights += 1;
                self.fruitful_nights = 0;
            }
            if self.enabled {
                self.apply_harass(turn);
            }
        }

        // B2. `Off` 档不是终身判决，是**试用期**：停买之后不会有新证据进来，所以
        //     到点必须自己重测一张（对方换边、换人、围墙被啃塌都会让门槛重新划算）。
        if self.enabled {
            if self.policy.harass == Harass::Off {
                self.off_nights += 1;
                if self.off_nights >= OFF_PROBATION_NIGHTS {
                    self.off_nights = 0;
                    self.policy.harass = Harass::Rhythm;
                    self.moved("harass", Harass::Off.as_str(), Harass::Rhythm.as_str(), "probation_retry", turn);
                }
            } else {
                self.off_nights = 0;
            }
        }

        // C. 火力缺口：估计有缺口 + 我方基地真挨打 = 估计被证实；连着零损失 =
        //    固定排序够用。`gap_funding` 只在缺口为正时才有作用（economy.rs）。
        if night.our_station_lost > 0 {
            self.gap_breach_nights += 1;
            self.gap_quiet_nights = 0;
        } else if night.our_loss() == 0 {
            self.gap_quiet_nights += 1;
        }
        if self.enabled {
            self.apply_gap(turn);
        }

        self.last_night = night;
        log::event(
            "coach_night",
            json!({
                "round": turn.round_no,
                "day": turn.day,
                "night": ledger_json(&night),
                "gapPredicted": self.night_gap,
                "policy": policy_json(self.policy),
                "evidence": self.evidence_json(),
            }),
        );
    }

    fn apply_harass(&mut self, turn: &Turn) {
        if self.sterile_nights >= STERILE_TO_DOWNGRADE {
            let next = match self.policy.harass {
                Harass::Rich => Harass::Rhythm,
                Harass::Rhythm => Harass::Off,
                Harass::Off => Harass::Off,
            };
            if next != self.policy.harass {
                let from = self.policy.harass;
                self.policy.harass = next;
                self.sterile_nights = 0;
                self.moved("harass", from.as_str(), next.as_str(), "summons_sterile", turn);
            }
        } else if self.fruitful_nights >= FRUITFUL_TO_UPGRADE {
            let next = match self.policy.harass {
                Harass::Off => Harass::Rhythm,
                Harass::Rhythm => Harass::Rich,
                Harass::Rich => Harass::Rich,
            };
            if next != self.policy.harass {
                let from = self.policy.harass;
                self.policy.harass = next;
                self.fruitful_nights = 0;
                self.moved("harass", from.as_str(), next.as_str(), "summons_bite", turn);
            }
        }
    }

    fn apply_gap(&mut self, turn: &Turn) {
        if !self.policy.gap_funding && self.gap_breach_nights > 0 {
            self.policy.gap_funding = true;
            self.moved("gap_funding", "off", "on", "station_breached", turn);
        } else if self.policy.gap_funding && self.gap_quiet_nights >= QUIET_TO_RELAX_GAP {
            self.policy.gap_funding = false;
            self.moved("gap_funding", "on", "off", "two_quiet_nights", turn);
        }
    }

    fn set_station_pressure(&mut self, on: bool, why: &str, turn: &Turn) {
        let from = self.policy.station_pressure;
        self.policy.station_pressure = on;
        self.moved("station_pressure", from, on, why, turn);
    }

    fn moved(&self, switch: &str, from: impl std::fmt::Display, to: impl std::fmt::Display, why: &str, turn: &Turn) {
        log::event(
            "coach_move",
            json!({
                "round": turn.round_no,
                "day": turn.day,
                "switch": switch,
                "from": from.to_string(),
                "to": to.to_string(),
                "why": why,
                "night": ledger_json(&self.ledger),
                "evidence": self.evidence_json(),
            }),
        );
        // 每次移动档位顺手落盘。半场结束时当然也会写，但判决机若**每半场重启一次
        // 进程**，`state.rs` 就检测不到 roundNo 回退，`end_half` 永远不会被调用——
        // 那时这条就是唯一把学习结果带出本场的机会。写盘尽力而为，测试里
        // `persist == false`，是纯粹的空操作。
        self.save();
    }

    /// 计数器快照（只读）。
    pub fn counters(&self) -> Counters {
        Counters {
            bleed_nights: self.bleed_nights,
            quiet_nights: self.quiet_nights,
            sterile_nights: self.sterile_nights,
            fruitful_nights: self.fruitful_nights,
            gap_breach_nights: self.gap_breach_nights,
            gap_quiet_nights: self.gap_quiet_nights,
            orders_from_day: self.orders_from_day,
            orders_from_night: self.orders_from_night,
        }
    }

    fn evidence_json(&self) -> Value {
        let counters = self.counters();
        json!({
            "bleedNights": counters.bleed_nights,
            "quietNights": counters.quiet_nights,
            "sterile": counters.sterile_nights,
            "fruitful": counters.fruitful_nights,
            "gapBreach": counters.gap_breach_nights,
            "gapQuiet": counters.gap_quiet_nights,
            "ordersFromDay": counters.orders_from_day,
            "ordersFromNight": counters.orders_from_night,
            "offNights": self.off_nights,
        })
    }

    /// 半场结束（`state.rs` 检测到 roundNo 回退时调用）。
    ///
    /// 半场的最后一夜通常没有"夜晚→白天"边沿（一场比赛在第 2 天的夜里结束），
    /// 所以未结算的账在这里强制结算一次，然后证据衰减一半跨半场继承。
    pub fn end_half(&mut self, day: i64, round: i64) {
        if !self.ledger.idle() {
            let night = std::mem::take(&mut self.ledger);
            self.last_night = night;
            if night.our_station_lost > 0 && night.their_station_lost == 0 {
                self.bleed_nights += 1;
            }
            if self.orders_from_day > 0 && !night.our_fire_on_enemy {
                if night.their_loss() > 0 {
                    self.fruitful_nights += 1;
                    self.sterile_nights = 0;
                } else {
                    self.sterile_nights += 1;
                    self.fruitful_nights = 0;
                }
            }
        }
        let (bleed, quiet, sterile, fruitful, breach, gap_quiet) = (
            self.bleed_nights,
            self.quiet_nights,
            self.sterile_nights,
            self.fruitful_nights,
            self.gap_breach_nights,
            self.gap_quiet_nights,
        );
        self.halves += 1;
        // 衰减：跨半场的证据只是让下一个半场"更快得出结论"，绝不能单独成为结论。
        self.bleed_nights = (bleed / CARRY_DECAY).max(0);
        self.quiet_nights = quiet / CARRY_DECAY;
        self.sterile_nights = sterile / CARRY_DECAY;
        self.fruitful_nights = fruitful / CARRY_DECAY;
        self.gap_breach_nights = breach / CARRY_DECAY;
        self.gap_quiet_nights = gap_quiet / CARRY_DECAY;
        self.prev = None;
        self.last_round = 0;
        self.last_in_day = None;
        self.orders_from_day = 0;
        self.orders_from_night = 0;
        // 试用期时钟归零：新半场的前几夜金币要留给塔，不是试单的时候，从零开始数。
        self.off_nights = 0;
        self.orders_seen_today = 0;
        self.night_gap = 0;
        self.ledger = NightLedger::default();
        log::event(
            "coach_half",
            json!({
                "round": round,
                "day": day,
                "halves": self.halves,
                "night": ledger_json(&self.last_night),
                "policy": policy_json(self.policy),
                "evidence": self.evidence_json(),
            }),
        );
        self.save();
    }

    /// 写盘（尽力而为）：先写临时文件再改名，避免并行进程读到半截 JSON。
    fn save(&self) {
        if !self.persist || !self.enabled {
            return;
        }
        let Some(path) = learn_path() else {
            return;
        };
        let payload = self.to_json().to_string();
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, payload).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "version": 1,
            "halves": self.halves,
            "policy": {
                "station_pressure": self.policy.station_pressure,
                "gap_funding": self.policy.gap_funding,
                "harass": self.policy.harass.as_str(),
            },
            "evidence": {
                "bleedNights": self.bleed_nights,
                "quietNights": self.quiet_nights,
                "sterile": self.sterile_nights,
                "fruitful": self.fruitful_nights,
                "gapBreach": self.gap_breach_nights,
                "gapQuiet": self.gap_quiet_nights,
            },
        })
    }

    fn from_json(value: &Value, persist: bool) -> Self {
        let mut coach = Self {
            persist,
            ..Self::default()
        };
        let read = |key: &str| value.get(key).and_then(Value::as_i64).unwrap_or(0);
        coach.halves = read("halves");
        let evidence = value.get("evidence").unwrap_or(&Value::Null);
        let read_evidence = |key: &str| evidence.get(key).and_then(Value::as_i64).unwrap_or(0);
        coach.bleed_nights = read_evidence("bleedNights").max(0);
        coach.quiet_nights = read_evidence("quietNights").max(0);
        coach.sterile_nights = read_evidence("sterile").max(0);
        coach.fruitful_nights = read_evidence("fruitful").max(0);
        coach.gap_breach_nights = read_evidence("gapBreach").max(0);
        coach.gap_quiet_nights = read_evidence("gapQuiet").max(0);
        let policy = value.get("policy").unwrap_or(&Value::Null);
        if let Some(on) = policy.get("station_pressure").and_then(Value::as_bool) {
            coach.policy.station_pressure = on;
        }
        if let Some(on) = policy.get("gap_funding").and_then(Value::as_bool) {
            coach.policy.gap_funding = on;
        }
        if let Some(harass) = policy
            .get("harass")
            .and_then(Value::as_str)
            .and_then(Harass::parse)
        {
            coach.policy.harass = harass;
        }
        coach
    }
}

fn ledger_json(night: &NightLedger) -> Value {
    json!({
        "ourStationLost": night.our_station_lost,
        "theirStationLost": night.their_station_lost,
        "ourWallLost": night.our_wall_lost,
        "theirWallLost": night.their_wall_lost,
        "ordersUsed": night.orders_used,
        "ourFireOnEnemy": night.our_fire_on_enemy,
    })
}

fn policy_json(policy: Policy) -> Value {
    json!({
        "stationPressure": policy.station_pressure,
        "gapFunding": policy.gap_funding,
        "harass": policy.harass.as_str(),
    })
}
