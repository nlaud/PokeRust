# TODO
### Fixes
- Toxic spikes from toxic debris are set on wrong side (presumable also applies to other spikes and poison spike setting points? P1's Glimmora's Toxic Debris!

Toxic Spikes (1) was set on P1's side!)
- Re-activating sand stream resets the weather duration to 5 again (it should stay the same and continue decrementing at end of turn). Make sure the inference engine also supports this.
- Yawn putting to sleep should be a reaction to the yawn ending
- Sleep should display as a reaction to the move, instead of silently not doing anything, same for other failure reactions
- Hospitatlity does not show healing or something?
### New features
- Hospitality should not reveal if your partner is full hp
- Frontend features
  - We need a favicon lul
  - Favorited teams should show up first in the teams dropdown
  - Other information modes such as open team sheets, open team sheets - natures (plan in depth how to implement the representation of this information, that's the hardest part)
  - Actually implement custom theme
  - EV Display should be in SP not EV
  - Instead of doing nothing when you click a team, the default behavior should be to begin editing the team. Remove the edit button for this as well. Same thing for formats, allow for format editing as well.
  - "Custom theme" should actually be custom, with color pickers for background and acecnt color, with text picked automatically to be visible
  - Music within settings w/ youtube api (VanilluxePavillion?)
    - There should be two playlists, one for battle music (https://www.youtube.com/watch?v=3KyqUee895Y&list=PL6uHbR5DF8jBKITHMx8hwgR0WDz6q7rgt) and one for ambient music (https://www.youtube.com/watch?v=TYdZmrpz7K0&list=PL6uHbR5DF8jBFrkhA7-8YQ2K5GlxdeMmP). There should be an in-site player within the settings menu (full width of that tab) which includes a skip, pause, and next buttons displayed on top of the video, as well as a thing so you can choose the place in the current songe, but without any other buttons. Above this player should be a volume slider that when clicked on the volume icon to the left mutes (changing the icon) but otherwise controls the volume of the current song. This should be at the bottom of the settings page, forced to the bottom of the screen.
    - This should play the ambient one by default but when a battle is in progress it should seamlessly fade the audio to the battle one, then fade back once its over. These playlists should both be constantly shuffled
    - Implement this with the official API for youtube, no shenanigans
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
