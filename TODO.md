# TODO
### Fixes
- Illusion wears off on immune hits and self targeting moves. Research this ability extensively before implementing. https://bulbapedia.bulbagarden.net/wiki/Illusion_(Ability)
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
