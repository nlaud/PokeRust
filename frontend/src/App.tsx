import { Route, Routes } from 'react-router-dom'
import NavBar from './components/layout/NavBar'
import SettingsSidebar from './components/layout/SettingsSidebar'
import SimulatePage from './pages/SimulatePage'
import TeamsPage from './pages/TeamsPage'
import FormatsPage from './pages/FormatsPage'

export default function App() {
  return (
    <div className="min-h-screen">
      <NavBar />
      <main>
        <Routes>
          <Route path="/" element={<TeamsPage />} />
          <Route path="/formats" element={<FormatsPage />} />
          <Route path="/simulate" element={<SimulatePage />} />
        </Routes>
      </main>
      <SettingsSidebar />
    </div>
  )
}
