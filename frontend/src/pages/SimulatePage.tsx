import { useEffect } from 'react'
import { storedBattleId, useBattle } from '../store/battleStore'
import SetupPanel from './simulate/SetupPanel'
import BattleScreen from './simulate/BattleScreen'

export default function SimulatePage() {
  const { battleId, view, restore } = useBattle()

  // Reconnect to an in-progress battle after a page refresh.
  useEffect(() => {
    if (!battleId) {
      const stored = storedBattleId()
      if (stored) void restore(stored)
    }
  }, [battleId, restore])

  const inBattle = !!battleId && !!view
  return inBattle ? <BattleScreen /> : <SetupPanel />
}
