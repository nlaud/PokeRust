# TODO
### Fixes
- Confusion damage should be a reaction to taking the damage
- Rage powder does not work properly (also follow me ??? P1's Sinistcha used Rage Powder!

P2's Aerodactyl used Dual Wingbeat!

The attack missed P1's Tyranitar-Mega!

P1's Tyranitar-Mega took damage (now 192 HP)

Hit 1 time(s)!)
- Same case, cannot miss a hit of an attack then hit the next one, once one misses all future multstrike hits miss
### New features
- Frontend features
  - Other information modes such as open team sheets, open team sheets - natures (plan in depth how to implement the representation of this information, that's the hardest part)
  - Music within settings w/ youtube api (VanilluxePavillion?)
- Eventually create nash solver and recursive evaluation (When both players have perfect information)
- Create a meta sampler from pikalytics, and then get the algorithm to understand that
  - [ ] Tracker page: needs a parser for lines of input -> action / reaction tree
        (figuring out what is a reaction to what and what causes what from just the
        lines, also must add guaranteed effects so the user doesn't need to put those
        in manually). This will likely include some RegEx stuff as well. Need a
        detailed spec.
    - https://stitch.withgoogle.com/projects/6512361286860616575 for designs
    - https://github.com/PokeAPI/sprites for FE sprites (fetched at runtime, never committed)
  - [ ] Then move on to actual bot creation, battle and mentor pages.
