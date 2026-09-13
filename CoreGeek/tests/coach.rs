//! 内置教练：不靠环境变量、不靠外部 workflow 的在线自适应。
//!
//! 内网 workflow 不能设置 `CG_TUNE_*`（见 `docs/WORKFLOW_REQUEST.md` 请求七），
//! 所以"哪一档更好"必须在进程内部从局势里读出来。这些用例逐条钉住三条闭环的
//! **证据 → 动作 → 反证据 → 复原**，以及"承诺默认档 = 今天的盘面行为"。

use serde_json::{json, Value};

use coregeek::brain::coach::{Coach, Harass, Policy};
use coregeek::brain::combat::{choose_attack_kind_with, init_sim, TargetKind};
use coregeek::model::{Turn, UnitKind, DAY_ROUNDS};
use coregeek::protocol::{Pos, Request};
use coregeek::state::BotState;

fn turn_from(payload: Value) -> Turn {
    let req: Request = serde_json::from_value(payload).expect("payload parses");
    Turn::from_request(req)
}

fn station(id: i64, x: i32, y: i32, hp: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "station",
        "health": hp, "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn wall(id: i64, x: i32, y: i32, hp: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "wall",
        "health": hp, "attackPower": 0, "attackRange": 0,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

fn rocket(id: i64, x: i32, y: i32, level: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "rocket",
        "health": 1000, "attackPower": 20, "attackRange": 0,
        "level": level, "backPackCapability": 0, "backpack": []
    })
}

fn gatling(id: i64, x: i32, y: i32, hp: i64) -> Value {
    json!({
        "id": id, "pos": {"x": x, "y": y}, "roleType": "gatling",
        "health": hp, "attackPower": 10, "attackRange": 3,
        "level": 1, "backPackCapability": 0, "backpack": []
    })
}

/// 血量向量：(我方基地, 对方基地, 我方围墙, 对方围墙)。围墙 0 血 = 没有围墙
/// （`Turn::walls` 只数活着的）。
type Hp = (i64, i64, i64, i64);

/// 一回合的完整输入：血量、当日召唤令计数、我方炮塔这一回合是否打了对方建筑。
struct Round {
    hp: Hp,
    orders: i32,
    enemy_fire: bool,
}

impl Round {
    fn of(hp: Hp) -> Self {
        Self {
            hp,
            orders: 0,
            enemy_fire: false,
        }
    }

    fn orders(mut self, orders: i32) -> Self {
        self.orders = orders;
        self
    }

    fn enemy_fire(mut self) -> Self {
        self.enemy_fire = true;
        self
    }
}

fn board(round: i64, hp: Hp) -> Turn {
    let (our_station, their_station, our_wall, their_wall) = hp;
    let mut ours = vec![station(10001, 10, 24, our_station)];
    if our_wall > 0 {
        ours.push(wall(11000, 8, 20, our_wall));
    }
    let mut theirs = vec![station(20001, 30, 6, their_station)];
    if their_wall > 0 {
        theirs.push(wall(21000, 28, 4, their_wall));
    }
    turn_from(json!({
        "roundNo": round,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {
            "type": "challenger", "goldNum": 0, "totalScore": 0,
            "playerTasks": [], "roles": ours
        },
        "teamEnemy": {"roles": theirs},
        "robot": {"roles": []},
    }))
}

/// 第 `day` 天的 60 个夜回合 + 天亮后的第一个回合（夜晚在这里结算）。
///
/// 回合号必须**连续**：教练只对相邻回合之间掉的血归因，所以用例走真实时间线，
/// 而不是跳着喂快照。
fn night_of(day: i64) -> std::ops::RangeInclusive<i64> {
    let base = (day - 1) * 130;
    base + DAY_ROUNDS + 1..=base + DAY_ROUNDS + 61
}

/// 第 `day` 天**整天**：白天 70 个回合 + 夜里 60 个回合 + 天亮那一回合。
fn whole_day(day: i64) -> std::ops::RangeInclusive<i64> {
    let base = (day - 1) * 130;
    base + 1..=base + 131
}

/// 沿时间线逐回合喂给教练。
fn run(coach: &mut Coach, rounds: std::ops::RangeInclusive<i64>, curve: impl Fn(i64) -> Round) {
    for round in rounds {
        let step = curve(round);
        coach.observe(&board(round, step.hp), step.orders);
        if step.enemy_fire {
            coach.note_enemy_fire();
        }
    }
}

/// 一整夜保持同一块盘面；召唤令计数在天亮那一回合归零（`state.rs` 的日翻转）。
fn steady(hp: Hp, orders: i32) -> impl Fn(i64) -> Round {
    move |round| {
        let day_open = (round - 1) % 130 == 0;
        Round::of(hp).orders(if day_open { 0 } else { orders })
    }
}

/// 一个昼夜的曲线：白天（含日翻转那一回合）用 `day` 这块盘面，夜里用 `night(偏移)`。
///
/// 令在白天第一个回合之后下出去（`day.rs` 第 12 步就是白天下单），计数在天亮那一
/// 回合归零——与 `state.rs` 的日翻转逐字一致。`night` 的偏移是 `in_day_round`
/// （`DAY_ROUNDS..130`）。
fn day_cycle(day: Hp, orders: i32, night: impl Fn(i64) -> Hp) -> impl Fn(i64) -> Round {
    move |round| {
        let in_day = (round - 1) % 130;
        if in_day == 0 {
            Round::of(day)
        } else if in_day < DAY_ROUNDS {
            Round::of(day).orders(orders)
        } else {
            Round::of(night(in_day)).orders(orders)
        }
    }
}

fn fired_at_station(turn: &Turn, targets: &[Pos]) -> bool {
    let footprint = turn
        .enemy
        .iter()
        .find(|unit| unit.kind == UnitKind::Station)
        .expect("enemy station")
        .footprint();
    targets.iter().all(|target| footprint.contains(target))
}

#[test]
fn the_startup_payload_reports_the_policy_and_the_halves_learned() {
    // `main.rs` 用**一次** `BotState::locked()` 取出这份载荷：std 的 `Mutex`
    // 不可重入，同一句里取两次就是启动即死锁。载荷形状必须与文档里的一致。
    let value = Coach::default().ready_json();
    assert_eq!(value["policy"]["stationPressure"], json!(true));
    assert_eq!(value["policy"]["gapFunding"], json!(false));
    assert_eq!(value["policy"]["harass"], json!("Rhythm"));
    assert_eq!(value["halvesLearned"], json!(0));
}

#[test]
fn the_committed_policy_is_the_shipped_behaviour() {
    let policy = Policy::committed();
    assert!(policy.station_pressure, "P2-2 压制默认开");
    assert!(!policy.gap_funding, "P1-1 资金重排默认关");
    assert_eq!(policy.harass, Harass::Rhythm, "召唤令默认按既有节奏");
    // 门槛逐字等于 economy.rs 里承诺的那两条。
    assert_eq!(Harass::Rhythm.thin_wall_limit(), 3);
    assert_eq!(Harass::Rhythm.stock_cap(), 2);
    assert_eq!(Harass::Off.stock_cap(), 0);
}

#[test]
fn our_station_bleeding_takes_the_guns_off_their_base_immediately() {
    let mut coach = Coach::default();
    let start = *night_of(1).start();
    run(&mut coach, night_of(1), |round| {
        // 夜里第 2 回合起基地开始掉血，对方基地毫发无损。
        if round == start {
            Round::of((1500, 1500, 0, 0))
        } else {
            Round::of((1200, 1500, 0, 0))
        }
    });
    assert!(
        !coach.policy().station_pressure,
        "我方基地在掉血、对方没有 → 当晚就收火，不等夜晚结算"
    );
    assert_eq!(coach.counters().bleed_nights, 1, "这一夜记进证据");
}

#[test]
fn the_guns_re_arm_only_after_two_quiet_nights() {
    let mut coach = Coach::default();
    let start = *night_of(1).start();
    run(&mut coach, night_of(1), |round| {
        if round == start {
            Round::of((1500, 1500, 0, 0))
        } else {
            Round::of((1200, 1500, 0, 0))
        }
    });
    assert!(!coach.policy().station_pressure);

    run(&mut coach, night_of(2), steady((1200, 1500, 0, 0), 0));
    assert!(!coach.policy().station_pressure, "一夜安静不足以推翻结论");
    run(&mut coach, night_of(3), steady((1200, 1500, 0, 0), 0));
    assert!(
        coach.policy().station_pressure,
        "连着两夜毫发无损 → 对方火力已经不在我们头上，重新开火"
    );
}

#[test]
fn two_wasted_summon_orders_downgrade_the_rhythm() {
    let mut coach = Coach::default();
    // 白天下的令，当晚就是它的效果夜（任务书 4.6 的"下个夜晚"）：对方毫发无损。
    run(
        &mut coach,
        whole_day(1),
        day_cycle((1500, 1500, 0, 0), 1, |_| (1500, 1500, 0, 0)),
    );
    assert_eq!(
        coach.policy().harass,
        Harass::Rhythm,
        "第一次只是样本：一夜不足以定论"
    );
    assert_eq!(coach.counters().sterile_nights, 1);

    run(
        &mut coach,
        whole_day(2),
        day_cycle((1500, 1500, 0, 0), 1, |_| (1500, 1500, 0, 0)),
    );
    assert_eq!(
        coach.policy().harass,
        Harass::Off,
        "连着两次令没咬动对方 → 不再花 200 金买空气"
    );
    assert_eq!(Harass::Off.stock_cap(), 0, "Off 档一张也不买");
}

#[test]
fn a_summon_that_bites_upgrades_the_rhythm() {
    // 令给"对方下个夜晚"加机器人（任务书 4.6）：白天下的单，效果就落在当晚——
    // 对方围墙在后半夜被啃掉一截。
    let biting = |in_day: i64| {
        if in_day >= 100 {
            (1500, 1500, 0, 400)
        } else {
            (1500, 1500, 0, 1000)
        }
    };
    let mut coach = Coach::default();
    run(&mut coach, whole_day(1), day_cycle((1500, 1500, 0, 1000), 1, biting));
    assert_eq!(coach.counters().fruitful_nights, 1, "令确实咬动了对方");

    run(&mut coach, whole_day(2), day_cycle((1500, 1500, 0, 1000), 1, biting));
    assert_eq!(
        coach.policy().harass,
        Harass::Rich,
        "连着两夜见效 → 加码"
    );
    assert_eq!(Harass::Rich.stock_cap(), 3);
    assert_eq!(Harass::Rich.thin_wall_limit(), 5);
}

/// 一天下来对方毫发无损——用来观察"令花得值不值"。
fn quiet_cycle(orders: i32) -> impl Fn(i64) -> Round {
    day_cycle((1500, 1500, 0, 0), orders, |_| (1500, 1500, 0, 0))
}

#[test]
fn an_off_rhythm_is_on_probation_and_re_tests_itself() {
    // `Off` 是个死胡同：不买单就永远拿不到"令见效"的证据，档位会永久卡死。所以
    // 停买若干夜之后必须自己放一张单出去重测——对手可能换了，对方围墙也可能已经
    // 被啃塌了，同一个门槛就重新划算了。
    let mut coach = Coach::default();
    run(&mut coach, whole_day(1), quiet_cycle(1));
    run(&mut coach, whole_day(2), quiet_cycle(1));
    assert_eq!(coach.policy().harass, Harass::Off, "两夜白花 → 停买");

    // `Off` 档 economy.rs 一张也不买（`stock_cap() == 0`），所以后面的白天不再下单。
    for day in 3..5 {
        run(&mut coach, whole_day(day), quiet_cycle(0));
        assert_eq!(
            coach.policy().harass,
            Harass::Off,
            "第 {day} 天还在试用期里，不反复试单"
        );
    }
    run(&mut coach, whole_day(5), quiet_cycle(0));
    assert_eq!(
        coach.policy().harass,
        Harass::Rhythm,
        "试用期到点 → 再买一张看看对方是不是还是那么硬"
    );
}

#[test]
fn an_order_placed_at_night_waits_for_the_night_after() {
    // 夜里下的单，效果在**再下一夜**：当夜对方掉的血是上一夜那批机器人打的，
    // 不能算到这单头上。
    let mut coach = Coach::default();
    run(&mut coach, night_of(1), steady((1500, 1500, 0, 1000), 1));
    // 夜 1 的账：白天没下单（白天没走），夜里那一单要等夜 2。
    assert_eq!(
        coach.counters().sterile_nights,
        0,
        "夜里下的令当夜不算账，哪怕对方当夜掉了血"
    );
    assert_eq!(coach.counters().orders_from_day, 1, "滚到下一夜等结算");
    assert_eq!(coach.counters().orders_from_night, 0);

    // 夜 2 对方毫发无损 → 这才是那一单的答卷：白花了。
    run(&mut coach, night_of(2), steady((1500, 1500, 0, 0), 0));
    assert_eq!(coach.counters().sterile_nights, 1, "轮到它交卷了");
}

#[test]
fn our_own_guns_on_their_buildings_do_not_credit_the_summons() {
    // 对方基地在掉血，但那是我们自己炮塔打的（night.rs 的 enemyFire）——
    // 白天下的令这一夜就是它的效果夜，说不清是谁打掉的，样本只能作废：
    // 既不算见效，也不算白花。
    let mut coach = Coach::default();
    let curve = day_cycle((1500, 1500, 0, 0), 1, |_| (1500, 900, 0, 0));
    run(&mut coach, whole_day(1), |round| {
        let step = curve(round);
        if (round - 1) % 130 >= DAY_ROUNDS {
            step.enemy_fire()
        } else {
            step
        }
    });
    let counters = coach.counters();
    assert_eq!(counters.sterile_nights, 0, "不算白花");
    assert_eq!(counters.fruitful_nights, 0, "也不算见效");
    assert_eq!(counters.pending_orders(), 0, "这一单的样本作废，不留给下一夜");
    assert_eq!(coach.policy().harass, Harass::Rhythm, "档位不动");
}

#[test]
fn a_breached_station_arms_the_gap_funding_the_dial_was_waiting_for() {
    let mut coach = Coach::default();
    assert!(!coach.policy().gap_funding, "承诺默认关闭");
    let start = *night_of(1).start();
    run(&mut coach, night_of(1), |round| {
        // 围墙先被啃，随后基地挨打：火力缺口的估计被现实证实。
        let offset = round - start;
        let hp = if offset == 0 {
            (1500, 1500, 1000, 0)
        } else if offset < 20 {
            (1500, 1500, 700, 0)
        } else {
            (1400, 1500, 700, 0)
        };
        Round::of(hp)
    });
    assert!(coach.policy().gap_funding, "基地挨过打 → 按缺口排序");
}

#[test]
fn two_untouched_nights_stand_the_gap_funding_down_again() {
    let mut coach = Coach::default();
    let start = *night_of(1).start();
    run(&mut coach, night_of(1), |round| {
        let hp = if round > start {
            (1400, 1500, 1000, 0)
        } else {
            (1500, 1500, 1000, 0)
        };
        Round::of(hp)
    });
    assert!(coach.policy().gap_funding);

    run(&mut coach, night_of(2), steady((1400, 1500, 1000, 0), 0));
    run(&mut coach, night_of(3), steady((1400, 1500, 1000, 0), 0));
    assert!(
        !coach.policy().gap_funding,
        "连着两夜零损失 → 固定排序够用，回到承诺档"
    );
}

#[test]
fn the_coach_survives_the_half_reset_and_hands_its_evidence_down() {
    let mut state = BotState::default();
    state.observe(&board(250, (1500, 1500, 0, 0)));
    state.observe(&board(251, (900, 1500, 0, 0)));
    assert!(!state.coach.policy().station_pressure, "上半场学会收火");

    // 下半场重开：roundNo 回到 1，`BotState` 整个清空——教练必须活下来。
    state.observe(&board(1, (1500, 1500, 0, 0)));
    assert_eq!(state.coach.halves, 1, "半场计数跟着走");
    assert!(
        !state.coach.policy().station_pressure,
        "学到的谨慎带进下一半场，而不是从头再来"
    );
}

#[test]
fn the_policy_actually_changes_what_the_guns_shoot() {
    // 满图火箭 + 对方一座残血炮塔：承诺档压基地，收火档打那座炮塔。
    let turn = turn_from(json!({
        "roundNo": 85,
        "mapInfo": {"width": 41, "height": 32, "zones": []},
        "teamOur": {"type": "challenger", "goldNum": 0, "totalScore": 0, "playerTasks": [],
            "roles": [station(10001, 10, 24, 1500), rocket(10040, 10, 20, 3)]},
        "teamEnemy": {"roles": [station(20001, 30, 6, 1500), gatling(20002, 30, 10, 100)]},
        "robot": {"roles": []},
    }));
    let tower = turn.role_by_id(10040).unwrap();

    let mut sim = init_sim(&turn);
    let (targets, kind) = choose_attack_kind_with(&Policy::committed(), &turn, tower, &mut sim)
        .expect("承诺档有目标");
    assert_eq!(kind, TargetKind::EnemyAssets);
    assert!(fired_at_station(&turn, &targets), "承诺档：压基地 {targets:?}");

    let quiet = Policy {
        station_pressure: false,
        ..Policy::committed()
    };
    let mut sim = init_sim(&turn);
    let (targets, _kind) = choose_attack_kind_with(&quiet, &turn, tower, &mut sim)
        .expect("收火档仍然打对方资产，只是不碰基地");
    assert!(
        !fired_at_station(&turn, &targets),
        "收火 = 不碰对方基地，转打最有价值的单位 {targets:?}"
    );
}

#[test]
fn a_frozen_coach_moves_nothing() {
    let mut coach = Coach::frozen();
    let start = *night_of(1).start();
    run(&mut coach, night_of(1), |round| {
        if round == start {
            Round::of((1500, 1500, 0, 0))
        } else {
            Round::of((1200, 1500, 0, 0)).orders(1)
        }
    });
    run(&mut coach, night_of(2), steady((1200, 1500, 0, 0), 1));
    assert_eq!(coach.policy(), Policy::committed(), "CG_LEARN=0 → 档位冻结");
}
