#[derive(Debug)]
enum PokemonType{
    Normal,
    Fire,
    Water,
    Electric,
    Grass,
    Ice,
    Fighting,
    Poison,
    Ground,
    Flying,
    Psychic,
    Bug,
    Rock,
    Ghost,
    Dragon,
    Dark,
    Steel,
    Fairy
}

#[derive(Debug)]
enum PokemonAccuracy {
    True,
    Percent(u8)
}

#[derive(Debug)]
enum PokemonMoveCategory {
    Physical,
    Special,
    Status
}
#[derive(Debug)]
enum PokemonMoveTarget{
    AdjacentAlly,
    AdjacentAllyOrSelf,
    AdjacentFoe,
    All,
    AllAdjacent,
    AllAdjacentFoes,
    Allies,
    AllySide,
    AllyTeam,
    Any,
    FoeSide,
    Normal,
    RnadomNormal,
    Scripted,
    Self
}

#[derive(Debug)]
enum PokemonMoveFlags{
    BypassSub,
    Bite,
    Bullet,
    CantUseTwice,
    Charget,
    Contact,
    Dance,
    Defrost,
    Distance,
    FailyCopyCat,
    FailEncore,
    FailInstruct,
    FailMeFirst,
    FailMimic,
    FutureMove,
    Gravity,
    Heal,
    Metronome,
    Mirror,
    MustPressure,
    NoAssist,
    NoParentalBond,
    NoSketch,
    NoSleepTalk,
    PledgeCombo,
    Powder,
    Protect,
    Pulse,
    Punch,
    Recharge,
    Reflectable,
    Slicing,
    Snatch,
    Sound,
    Wind
}

#[derive(Debug)]
enum PokemonMoveDamage{
    Number(u16),
    Level,
    None
}

#[derive(Debug)]
enum PokemonSelfSwitchType{
    ShedTail,
    BatonPass,
    Normal,
    False
}

#[derive(Debug)]
enum PokemonSelfDestructType{
    Always,
    IfHit,
    False
}

enum PokemonNonVolatileStatus{
    Burn,
    Poison,
    ToxicPoison,
    Paralysis,
    Sleep,
    Frozen,
}

enum PokemonVolatileStatus{
    Flinch,
    AquaRing,
    Attract,
    Confusion,
    BanefulBunker,
    Bide,
    PartiallyTrapped,
    MustRecharge,
    BurningBulwark,
    Charge,
    Curse,
    DefenseCurl,
    DestinyBond,
    Protect,
    Disable,
    DragonCheer,
    Electrify,
    Embargo,
    Encore,
    Endure,
    FocusEnergy,
    FollowMe,
    Foresight,
    GastroAcid,
    GlaiveRush,
    Grudge,
    HealBlock,
    HelpingHand,
    Imprison,
    Ingrain,
    KingsShield,
    LaserFocus,
    LeechSeed,
    MagicCoat,
    MagnetRise,
    MaxGuard,
    Minimize,
    MiracleEye,
    NightMare,
    NoRetreat,
    Obstruct,
    OctoLock,
    LockedMove,
    Powder,
    PowerShift,
    PowerTrick,
    Protect,
    Rage,
    RagePowder,
    Roost,
    SaltCure,
    Substitute,
    SilkTrap,
    SmackDown,
    Snatch,
    SparklingAria,
    SpikyShield,
    Spotlight,
    Stockpile,
    SyrupBomb,
    TarShot,
    Taunt,
    Telekenesis,
    Torment,
    Uproar,
    Yawn,
}

#[derive(Debug)]
enum PokemonSideCondition{
    AuroraVeil,
    Reflect,
    CraftyShield,
    LightScreen,
    LuckyChant,
    MatBlock,
    Mist,
    QuickGuard,
    SafeGuard,
    Spikes,
    StealthRock,
    StickyWeb,
    TailWind,
    ToxicSpikes,
    WideGuard
}

#[derive(Debug)]
enum PokemonSlotCondition{
    FutureMove,
    HealingWish,
    LunarDance,
    RevivalBlessing,//???
    Wish
}

#[derive(Debug)]
enum PokemonPseudoWeather{
    FairyLock,
    Gravity,
    IonDeluge,
    MagicDeluge,
    MudSport,
    WaterSport,
    WonderRoom
}

#[derive(Debug)]
enum PokemonTerrain{
    ElectricTerrain,
    GrassyTerrain,
    MistyTerrain,
    PsychicTerrain,
}

#[derive(Debug)]
enum PokemonWeather{
    Rain,
    Sandstorm,
    Snow,
    Sun
}

#[derive(Debug)]
struct PokemonHitEffect{
    boosts: [i8, 6],
    status: PokemonNonVolatileStatus,
    volatile_status: PokemonVolatileStatus,
    slot_condition: PokemonSlotCondition,
    pseudo_weather: PokemonPseudoWeather,
    terrain: PokemonTerrain,
    weather: PokemonWeather
}

#[derive(Debug)]
struct PokemonSecondaryEffect{
    chance: u8,
    effect: PokemonHitEffect
}

#[derive(Debug)]
struct PokemonMoveData {
    name: String,

    accuracy: PokemonAccuracy,
    pp:u8,
    move_type: String,
    priority: i8,
    target: PokemonMoveTarget,
    flags: Vec<PokemonMoveFlags>,
    
    //Hit Effects
    ohko: bool,
    thaws_target: bool,
    heal_fraction: [u8; 2],
    force_switch: bool,
    self_switch: PokemonSelfSwitchType,
    self_boost: [i8; 6],
    self_destruct: PokemonSelfDestructType,
    breaks_protect: bool,
    recoil_fraction: [u8; 2],
    drain_fraction: [u8; 2],
    mind_blown_recoil: bool,
    struggle_recoil: bool,
    steals_boosts: bool,

    secondaries: Vec<PokemonSecondaryEffect>
    self_secondary: Option<PokemonSecondaryEffect>

    //Hit Modifiers
    crit_ratio: u8;
    foul_play: bool,

    //Other Mods
    ignore_ability:bool,
    ignore_defense_boosts:bool,
    ignore_evasion:bool,
    ignore_immunity: Vec<PokemonType>,

    multihit_range: [u8; 2],
    multihit_accuracy: bool,

    sleep_usable:bool,
    smart_target:bool,
    tracks_target:bool,
    calls_move:bool,//Calls for another move
    
    has_crash_damage:bool
}

#[derive(Debug)]
struct PokemonData {
    species: String,
    types: Vec<PokemonType>, 
    base_stats: [u16, 6],
    weight: u16,
}