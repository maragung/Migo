import React, { useState, useEffect, useRef } from 'react';
import { 
  User, Users, MessageSquare, Gift, Gamepad2, Bell, Mail, 
  Settings, HelpCircle, LogOut, ChevronDown, ChevronRight, 
  Search, RefreshCw, Volume2, VolumeX, Smartphone, Laptop, 
  Send, Smile, Shield, Crown, Play, CheckSquare, Square, X, PlusCircle, Sparkles,
  Dices, Hash, AtSign, Eye, EyeOff, CheckCheck, Zap, Award, Flame, Heart,
  Sparkle, Trophy, RotateCcw, HelpCircle as QuizIcon, Layers, Star, UserPlus,
  Edit3, Key, Info, CreditCard, Lock, Columns, ChevronLeft
} from 'lucide-react';

// --- Web Audio Sound Effects System ---
const playSound = (type) => {
  try {
    const ctx = new (window.AudioContext || window.webkitAudioContext)();
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.connect(gain);
    gain.connect(ctx.destination);

    if (type === 'click') {
      osc.type = 'sine';
      osc.frequency.setValueAtTime(600, ctx.currentTime);
      gain.gain.setValueAtTime(0.03, ctx.currentTime);
      gain.gain.exponentialRampToValueAtTime(0.001, ctx.currentTime + 0.05);
      osc.start();
      osc.stop(ctx.currentTime + 0.05);
    } else if (type === 'msg') {
      osc.type = 'sine';
      osc.frequency.setValueAtTime(523.25, ctx.currentTime);
      osc.frequency.setValueAtTime(659.25, ctx.currentTime + 0.08);
      gain.gain.setValueAtTime(0.06, ctx.currentTime);
      gain.gain.exponentialRampToValueAtTime(0.001, ctx.currentTime + 0.22);
      osc.start();
      osc.stop(ctx.currentTime + 0.22);
    } else if (type === 'dice') {
      osc.type = 'triangle';
      osc.frequency.setValueAtTime(320, ctx.currentTime);
      osc.frequency.linearRampToValueAtTime(160, ctx.currentTime + 0.15);
      gain.gain.setValueAtTime(0.08, ctx.currentTime);
      gain.gain.exponentialRampToValueAtTime(0.001, ctx.currentTime + 0.15);
      osc.start();
      osc.stop(ctx.currentTime + 0.15);
    } else if (type === 'egg') {
      osc.type = 'sine';
      osc.frequency.setValueAtTime(450, ctx.currentTime);
      osc.frequency.exponentialRampToValueAtTime(900, ctx.currentTime + 0.15);
      gain.gain.setValueAtTime(0.08, ctx.currentTime);
      gain.gain.exponentialRampToValueAtTime(0.001, ctx.currentTime + 0.2);
      osc.start();
      osc.stop(ctx.currentTime + 0.2);
    } else if (type === 'win') {
      osc.type = 'sine';
      osc.frequency.setValueAtTime(523.25, ctx.currentTime);
      osc.frequency.setValueAtTime(659.25, ctx.currentTime + 0.1);
      osc.frequency.setValueAtTime(783.99, ctx.currentTime + 0.2);
      gain.gain.setValueAtTime(0.1, ctx.currentTime);
      gain.gain.exponentialRampToValueAtTime(0.001, ctx.currentTime + 0.4);
      osc.start();
      osc.stop(ctx.currentTime + 0.4);
    }
  } catch (e) {
    // Audio Context disekat otomatis
  }
};

export default function App() {
  const [soundEnabled, setSoundEnabled] = useState(true);
  const [previewMode, setPreviewMode] = useState('pc'); // 'pc' | 'mobile'

  // Status Auth & Form View Mode
  const [authView, setAuthView] = useState('login');
  const [isLoggedIn, setIsLoggedIn] = useState(false);
  
  // Login State
  const [username, setUsername] = useState('reason007007');
  const [password, setPassword] = useState('••••••••');
  const [showPassword, setShowPassword] = useState(false);
  const [rememberMe, setRememberMe] = useState(true);
  const [loginInvisible, setLoginInvisible] = useState(false);

  // Register State
  const [regUsername, setRegUsername] = useState('');
  const [regEmail, setRegEmail] = useState('');
  const [regPassword, setRegPassword] = useState('');
  const [regConfirmPassword, setRegConfirmPassword] = useState('');

  // Avatar Dropdown Menu Modal & Sub-Modals
  const [showAvatarMenu, setShowAvatarMenu] = useState(false);
  const [activeModal, setActiveModal] = useState(null);

  // Sistem Tab Utama (Friends, Rooms, Games, Updates) + Tab Chat Dinamis
  const initialSystemTabs = [
    { id: 'friends', title: 'Friends', type: 'system', icon: Users },
    { id: 'rooms', title: 'Rooms', type: 'system', icon: MessageSquare },
    { id: 'games', title: 'Games', type: 'system', icon: Gamepad2 },
    { id: 'updates', title: 'Feed', type: 'system', icon: Sparkles },
  ];
  
  const [systemTabs, setSystemTabs] = useState(initialSystemTabs);
  const [activeTabId, setActiveTabId] = useState('friends');

  // Profile Data
  const [statusText, setStatusText] = useState('Euphoria Whisper 🎵');
  const [userStatus, setUserStatus] = useState('Available');
  const [eggCount, setEggCount] = useState(24);
  const [credits, setCredits] = useState(8500);
  const [userLevel, setUserLevel] = useState(14);
  const [userXp, setUserXp] = useState(72);

  // Search Filter Queries
  const [searchFriendQuery, setSearchFriendQuery] = useState('');
  const [searchRoomQuery, setSearchRoomQuery] = useState('');

  // Histori Obrolan Map: { tabId: [messages...] }
  const [chatHistories, setChatHistories] = useState({});
  const [chatInput, setChatInput] = useState('');

  // Popup Toast
  const [eggAnimation, setEggAnimation] = useState(null);

  // Data Teman
  const [friends, setFriends] = useState([
    { id: 1, name: 'reason008', status: 'online', isVip: true, mood: 'Main dice yuk!', avatarBg: 'bg-emerald-500', avatarIcon: '🤖' },
    { id: 2, name: 'nrock', status: 'online', isVip: false, mood: 'Listening to Linkin Park', avatarBg: 'bg-[#00BCD4]', avatarIcon: '🎧' },
    { id: 3, name: 'neel_the_great', status: 'online', isVip: true, mood: 'Salam kawan semua ✌️', avatarBg: 'bg-indigo-500', avatarIcon: '👑' },
    { id: 4, name: 'ahok', status: 'online', isVip: false, mood: 'Ada yang mau barter egg?', avatarBg: 'bg-amber-500', avatarIcon: '🥚' },
    { id: 5, name: 'sampit_gaul', status: 'offline', isVip: false, mood: 'Tidur dulu zzz...', avatarBg: 'bg-slate-400', avatarIcon: '😴' }
  ]);

  // Data Room Obrolan
  const [chatRooms, setChatRooms] = useState([
    { id: 'r1', name: 'sampit_terindah', users: 18, max: 30, category: 'Recent Rooms', badge: 'Popular' },
    { id: 'r2', name: 'indo_terindah', users: 24, max: 40, category: 'Recent Rooms', badge: 'Active' },
    { id: 'r3', name: 'malang_jomblo2', users: 12, max: 40, category: 'Recent Rooms', badge: 'Fun' },
    { id: 'r4', name: 'Jakarta_Gaul', users: 42, max: 50, category: 'Favorites', badge: 'Hot' },
    { id: 'r5', name: 'Cari_Jodoh_Nusantara', users: 38, max: 50, category: 'Favorites', badge: 'Top' }
  ]);

  // --- GAME CENTER STATES ---
  const [activeGame, setActiveGame] = useState(null);

  const [diceState, setDiceState] = useState({
    playerRoll: null,
    botRoll: null,
    result: null,
    bet: 500,
    isPlaying: false,
    wins: 3,
    losses: 1
  });

  const chatBottomRef = useRef(null);

  useEffect(() => {
    chatBottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [chatHistories, activeTabId]);

  const triggerFx = (type) => {
    if (soundEnabled) playSound(type);
  };

  const handleLogin = (e) => {
    if (e) e.preventDefault();
    triggerFx('click');
    setIsLoggedIn(true);
    if (loginInvisible) setUserStatus('Invisible');
  };

  const handleRegister = (e) => {
    e.preventDefault();
    if (regPassword !== regConfirmPassword) {
      alert('Password dan Konfirmasi Password tidak cocok!');
      return;
    }
    triggerFx('click');
    setUsername(regUsername || 'user_baru');
    setIsLoggedIn(true);
    setAuthView('login');
  };

  const handleLogout = () => {
    triggerFx('click');
    setIsLoggedIn(false);
    setShowAvatarMenu(false);
    setActiveModal(null);
    setSystemTabs(initialSystemTabs);
    setActiveTabId('friends');
    setActiveGame(null);
  };

  // Buka Room Obrolan sebagai Tab Baru di Sebelah Kanan Feed
  const handleOpenRoomTab = (room) => {
    triggerFx('click');
    const tabId = `room-${room.id}`;
    
    const exists = systemTabs.find(t => t.id === tabId);
    if (!exists) {
      const newTab = { id: tabId, title: `#${room.name}`, type: 'chatroom', roomData: room, closable: true };
      setSystemTabs(prev => [...prev, newTab]);

      if (!chatHistories[tabId]) {
        setChatHistories(prev => ({
          ...prev,
          [tabId]: [
            { id: 1, sender: 'System', text: `*** Selamat datang di room #${room.name} ***`, isSystem: true },
            { id: 2, sender: 'Bot', text: 'Halo gaes! Jaga kesopanan & patuhi rule room ya.', isBot: true },
            { id: 3, sender: 'reason008', text: `Halo @${username}! Selamat bergabung bro 🎉`, time: '11:32' }
          ]
        }));
      }
    }
    setActiveTabId(tabId);
  };

  // Buka PM sebagai Tab Baru di Sebelah Kanan Feed
  const handleOpenPMTab = (friendName) => {
    triggerFx('click');
    const tabId = `pm-${friendName}`;

    const exists = systemTabs.find(t => t.id === tabId);
    if (!exists) {
      const newTab = { id: tabId, title: `@${friendName}`, type: 'pm', targetName: friendName, closable: true };
      setSystemTabs(prev => [...prev, newTab]);

      if (!chatHistories[tabId]) {
        setChatHistories(prev => ({
          ...prev,
          [tabId]: [
            { id: 1, sender: 'System', text: `Sesi obrolan pribadi bersama ${friendName}`, isSystem: true },
            { id: 2, sender: friendName, text: 'Oi bro, lagi di mana?', time: '11:30' }
          ]
        }));
      }
    }
    setActiveTabId(tabId);
  };

  // Tutup Tab Chat
  const handleCloseChatTab = (e, tabIdToClose) => {
    e.stopPropagation();
    triggerFx('click');

    const nextTabs = systemTabs.filter(t => t.id !== tabIdToClose);
    setSystemTabs(nextTabs);
    if (activeTabId === tabIdToClose) {
      setActiveTabId(nextTabs[nextTabs.length - 1]?.id || 'friends');
    }
  };

  const handleSendMessage = (e) => {
    e.preventDefault();
    if (!chatInput.trim()) return;
    triggerFx('click');

    const newMsg = {
      id: Date.now(),
      sender: username,
      text: chatInput,
      time: new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    };

    setChatHistories(prev => ({
      ...prev,
      [activeTabId]: [...(prev[activeTabId] || []), newMsg]
    }));
    setChatInput('');

    setTimeout(() => {
      triggerFx('msg');
      const activeTabObj = systemTabs.find(t => t.id === activeTabId);
      const replySender = activeTabObj?.type === 'pm' ? activeTabObj.targetName : 'reason008';
      const replyMsg = {
        id: Date.now() + 1,
        sender: replySender,
        text: 'Mantap bro! 🥚 diterima dengan baik.',
        time: new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
      };
      setChatHistories(prev => ({
        ...prev,
        [activeTabId]: [...(prev[activeTabId] || []), replyMsg]
      }));
    }, 1200);
  };

  const handleSendEgg = () => {
    if (eggCount <= 0) return;
    triggerFx('egg');
    setEggCount(prev => prev - 1);
    setUserXp(prev => Math.min(100, prev + 10));

    const activeTabObj = systemTabs.find(t => t.id === activeTabId);
    setEggAnimation(`🥚 Melempar Telur ke ${activeTabObj?.title || 'Chat'}! (+10 EXP)`);
    setTimeout(() => setEggAnimation(null), 2000);

    setChatHistories(prev => ({
      ...prev,
      [activeTabId]: [
        ...(prev[activeTabId] || []),
        { id: Date.now(), sender: 'System', text: `🥚 ${username} melempar TELUR KELUARGA! (+10 Exp)`, isSystem: true }
      ]
    }));
  };

  const handleRollDice = () => {
    if (credits < diceState.bet) {
      alert('Kredit tidak cukup!');
      return;
    }
    triggerFx('dice');
    setDiceState(prev => ({ ...prev, isPlaying: true }));

    setTimeout(() => {
      const pRoll = Math.floor(Math.random() * 6) + 1;
      const bRoll = Math.floor(Math.random() * 6) + 1;
      let res = 'SERI';
      let change = 0;
      let newWins = diceState.wins;
      let newLosses = diceState.losses;

      if (pRoll > bRoll) {
        res = 'MENANG';
        change = diceState.bet;
        newWins += 1;
        triggerFx('win');
      } else if (pRoll < bRoll) {
        res = 'KALAH';
        change = -diceState.bet;
        newLosses += 1;
        triggerFx('msg');
      }

      setCredits(prev => prev + change);
      setDiceState({
        playerRoll: pRoll,
        botRoll: bRoll,
        result: res,
        bet: diceState.bet,
        isPlaying: false,
        wins: newWins,
        losses: newLosses
      });
    }, 700);
  };

  const filteredFriends = friends.filter(f => 
    f.name.toLowerCase().includes(searchFriendQuery.toLowerCase()) || 
    f.mood.toLowerCase().includes(searchFriendQuery.toLowerCase())
  );

  const activeTabObj = systemTabs.find(t => t.id === activeTabId);

  return (
    <div className="min-h-screen bg-slate-950 text-slate-100 flex flex-col items-center justify-center p-3 sm:p-6 font-sans select-none antialiased">
      
      {/* SWITCH PREVIEW PC / MOBILE VIEW */}
      <div className="w-full max-w-xl flex justify-between items-center mb-3 px-2">
        <span className="text-xs text-slate-400 font-mono">Chat Platform v4.6</span>
        <div className="flex items-center gap-1 bg-slate-900 border border-slate-800 p-1 rounded-xl shadow-inner">
          <button
            onClick={() => { triggerFx('click'); setPreviewMode('pc'); }}
            className={`flex items-center gap-1.5 px-3 py-1 rounded-lg text-xs font-semibold transition ${
              previewMode === 'pc' ? 'bg-cyan-600 text-white shadow' : 'text-slate-400 hover:text-white'
            }`}
          >
            <Laptop className="w-3.5 h-3.5" /> PC View
          </button>
          <button
            onClick={() => { triggerFx('click'); setPreviewMode('mobile'); }}
            className={`flex items-center gap-1.5 px-3 py-1 rounded-lg text-xs font-semibold transition ${
              previewMode === 'mobile' ? 'bg-cyan-600 text-white shadow' : 'text-slate-400 hover:text-white'
            }`}
          >
            <Smartphone className="w-3.5 h-3.5" /> Mobile View
          </button>
        </div>
      </div>

      {/* WINDOW UTAMA APLIKASI */}
      <div className={`w-full ${previewMode === 'pc' ? 'max-w-xl' : 'max-w-md'} bg-[#fdfbf7] text-slate-900 rounded-3xl border-2 border-slate-700 shadow-2xl flex flex-col min-h-[700px] max-h-[820px] overflow-hidden relative font-sans text-xs transition-all duration-300`}>
        
        {!isLoggedIn ? (
          /* LOGIN SCREEN */
          <div className="flex-1 bg-gradient-to-b from-[#0093AF] via-[#00ACC1] to-[#00838F] flex flex-col items-center justify-between p-6 sm:p-8 text-white relative overflow-hidden w-full">
            <div className="w-full flex justify-between items-center text-[11px] text-cyan-100 font-mono mb-2 z-10">
              <span className="flex items-center gap-1.5"><span className="w-2.5 h-2.5 rounded-full bg-emerald-400 animate-ping"></span>TELKOMSEL 3G</span>
              <span>11:33 🔋</span>
            </div>

            <div className="flex flex-col items-center my-auto z-10 w-full max-w-xs space-y-4">
              <div className="bg-[#00BCD4] border-2 border-white text-white px-8 py-3.5 rounded-3xl font-extrabold text-3xl shadow-xl tracking-wider flex items-center gap-2">
                <MessageSquare className="w-8 h-8 text-white" />
                <span>Chat</span>
              </div>

              <div className="flex items-end justify-center gap-2">
                <div className="w-8 h-8 bg-emerald-400 border-2 border-white rounded-t-full flex items-center justify-center text-sm shadow-md">🤖</div>
                <div className="w-10 h-10 bg-slate-100 border-2 border-slate-300 rounded-t-full flex items-center justify-center text-base shadow-lg">🤖</div>
                <div className="w-8 h-8 bg-pink-400 border-2 border-white rounded-t-full flex items-center justify-center text-sm shadow-md">🌸</div>
              </div>

              <p className="text-xs font-semibold text-cyan-100 tracking-wide">
                {authView === 'login' ? 'Join the Fun!' : 'Buat Akun Baru Sekarang'}
              </p>

              {authView === 'login' ? (
                <form onSubmit={handleLogin} className="w-full space-y-3 bg-white/15 p-5 rounded-2xl border border-white/30 backdrop-blur-md shadow-2xl">
                  <div>
                    <input
                      type="text"
                      value={username}
                      onChange={(e) => setUsername(e.target.value)}
                      placeholder="Username"
                      className="w-full px-3.5 py-2.5 text-slate-800 bg-white border border-cyan-200 rounded-xl shadow-inner focus:outline-none focus:ring-2 focus:ring-cyan-400 text-xs transition"
                      required
                    />
                  </div>
                  <div className="relative">
                    <input
                      type={showPassword ? 'text' : 'password'}
                      value={password}
                      onChange={(e) => setPassword(e.target.value)}
                      placeholder="Password"
                      className="w-full px-3.5 py-2.5 text-slate-800 bg-white border border-cyan-200 rounded-xl shadow-inner focus:outline-none focus:ring-2 focus:ring-cyan-400 text-xs pr-9 transition"
                      required
                    />
                    <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-3 text-slate-400 hover:text-cyan-700 transition">
                      {showPassword ? <EyeOff className="w-4 h-4" /> : <Eye className="w-4 h-4" />}
                    </button>
                  </div>
                  <button type="submit" className="w-full py-2.5 bg-gradient-to-r from-orange-500 to-amber-500 hover:from-orange-600 hover:to-amber-600 text-white font-bold rounded-xl shadow-lg border border-orange-300 active:scale-95 transition text-xs tracking-wider">
                    Go!
                  </button>
                </form>
              ) : (
                <form onSubmit={handleRegister} className="w-full space-y-2.5 bg-white/15 p-5 rounded-2xl border border-white/30 backdrop-blur-md shadow-2xl">
                  <div><input type="text" value={regUsername} onChange={(e) => setRegUsername(e.target.value)} placeholder="Username Baru" className="w-full px-3.5 py-2 text-slate-800 bg-white border border-cyan-200 rounded-xl text-xs" required /></div>
                  <div><input type="email" value={regEmail} onChange={(e) => setRegEmail(e.target.value)} placeholder="Alamat Email" className="w-full px-3.5 py-2 text-slate-800 bg-white border border-cyan-200 rounded-xl text-xs" required /></div>
                  <div><input type="password" value={regPassword} onChange={(e) => setRegPassword(e.target.value)} placeholder="Password Baru" className="w-full px-3.5 py-2 text-slate-800 bg-white border border-cyan-200 rounded-xl text-xs" required /></div>
                  <div><input type="password" value={regConfirmPassword} onChange={(e) => setRegConfirmPassword(e.target.value)} placeholder="Konfirmasi Password" className="w-full px-3.5 py-2 text-slate-800 bg-white border border-cyan-200 rounded-xl text-xs" required /></div>
                  <button type="submit" className="w-full py-2.5 bg-gradient-to-r from-emerald-500 to-teal-600 text-white font-bold rounded-xl text-xs">Daftar Sekarang</button>
                </form>
              )}
            </div>

            <div className="z-10 pb-2 text-center">
              {authView === 'login' ? (
                <button onClick={() => { triggerFx('click'); setAuthView('register'); }} className="text-cyan-100 text-xs underline font-semibold hover:text-white">Create Account</button>
              ) : (
                <button onClick={() => { triggerFx('click'); setAuthView('login'); }} className="text-cyan-100 text-xs underline font-semibold hover:text-white">Sudah Memiliki Akun? Login</button>
              )}
            </div>
          </div>
        ) : (
          /* WORKSPACE UTAMA SETELAH LOGIN (TAB BAR BERISI SYSTEM TABS + CHAT TABS YANG BISA DI-CLOSE) */
          <div className="flex-1 flex flex-col bg-[#fdfbf7] text-slate-900 overflow-hidden relative">
            
            {/* TAB BAR UTAMA (FRIENDS, ROOMS, GAMES, FEED, DAN TAB CHAT AKTIF) */}
            <div className="bg-[#00838F] text-white flex items-center text-[11px] font-semibold border-b border-cyan-900 shadow-sm overflow-x-auto no-scrollbar p-1.5 gap-1.5">
              {systemTabs.map((tab) => {
                const isActive = activeTabId === tab.id;
                const IconComponent = tab.icon;
                return (
                  <div
                    key={tab.id}
                    onClick={() => { triggerFx('click'); setActiveTabId(tab.id); }}
                    className={`py-1.5 px-2.5 rounded-xl flex items-center gap-1.5 cursor-pointer whitespace-nowrap transition-all duration-150 shrink-0 ${
                      isActive ? 'bg-[#00ACC1] text-white font-bold shadow-md border-b-2 border-orange-400' : 'bg-cyan-900/40 text-cyan-100 hover:bg-cyan-800/80'
                    }`}
                  >
                    {IconComponent && <IconComponent className="w-3.5 h-3.5 text-cyan-200" />}
                    {tab.type === 'chatroom' && <span className="text-amber-300 font-bold">💬</span>}
                    {tab.type === 'pm' && <span className="text-emerald-300 font-bold">👤</span>}

                    <span>{tab.title}</span>

                    {/* Tombol Tutup Tab Khusus Chat */}
                    {tab.closable && (
                      <button
                        onClick={(e) => handleCloseChatTab(e, tab.id)}
                        className="ml-1 p-0.5 hover:bg-cyan-900 rounded-full text-cyan-200 hover:text-white transition"
                        title="Tutup Tab"
                      >
                        <X className="w-3 h-3" />
                      </button>
                    )}
                  </div>
                );
              })}
            </div>

            {/* BANNER PROFIL ORANGE */}
            <div className="bg-gradient-to-r from-orange-600 via-orange-500 to-amber-500 text-white p-2 px-3 flex items-center gap-2.5 border-b border-orange-600 shadow-inner relative">
              <div className="relative">
                <button onClick={() => { triggerFx('click'); setShowAvatarMenu(!showAvatarMenu); }} className="w-9 h-9 bg-white rounded-xl border-2 border-orange-200 p-0.5 flex items-center justify-center shadow hover:scale-105 transition cursor-pointer">
                  <div className="w-full h-full bg-cyan-50 border border-cyan-200 rounded-lg flex items-center justify-center text-sm">🤖</div>
                </button>

                {showAvatarMenu && (
                  <div className="absolute top-11 left-0 w-52 bg-white text-slate-800 rounded-2xl shadow-2xl border border-slate-200 p-1.5 z-50">
                    <div className="p-2 bg-slate-50 border-b border-slate-100 rounded-xl mb-1">
                      <p className="font-bold text-xs text-slate-800">{username}</p>
                      <p className="text-[10px] text-slate-500">Status: {userStatus}</p>
                    </div>
                    <button onClick={() => { setActiveModal('profile'); setShowAvatarMenu(false); }} className="w-full text-left px-3 py-1.5 text-xs font-semibold text-slate-700 hover:bg-cyan-50 rounded-xl flex items-center gap-2">
                      <User className="w-3.5 h-3.5 text-cyan-600" /> My Profile
                    </button>
                    <button onClick={() => { setActiveModal('topup'); setShowAvatarMenu(false); }} className="w-full text-left px-3 py-1.5 text-xs font-semibold text-slate-700 hover:bg-cyan-50 rounded-xl flex items-center gap-2">
                      <CreditCard className="w-3.5 h-3.5 text-amber-600" /> My Credits & TopUp
                    </button>
                    <div className="border-t border-slate-100 my-1"></div>
                    <button onClick={handleLogout} className="w-full text-left px-3 py-1.5 text-xs font-bold text-rose-600 hover:bg-rose-50 rounded-xl flex items-center gap-2">
                      <LogOut className="w-3.5 h-3.5 text-rose-600" /> Exit / Logout
                    </button>
                  </div>
                )}
              </div>

              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-1.5">
                  <span className="w-2 h-2 bg-emerald-400 rounded-full"></span>
                  <span className="font-bold text-xs truncate">{username}</span>
                </div>
                <p className="text-[10px] text-orange-100 truncate italic">{statusText}</p>
              </div>

              <div className="bg-orange-700/40 border border-orange-300/40 rounded-xl px-2 py-1 text-center shrink-0 flex items-center gap-2">
                <span className="text-[11px] font-bold text-amber-200">🥚 {eggCount}</span>
              </div>
            </div>

            {/* KONTEN TAB AKTIF (BISA BERUPA FRIENDS, ROOMS, GAMES, FEED, ATAU CHAT TAB) */}
            <div className="flex-1 overflow-y-auto bg-white flex flex-col">
              {activeTabId === 'friends' && (
                <div className="divide-y divide-slate-100">
                  <div className="p-2.5 bg-slate-50 border-b border-slate-200 flex items-center gap-2">
                    <Search className="w-4 h-4 text-slate-400" />
                    <input type="text" value={searchFriendQuery} onChange={(e) => setSearchFriendQuery(e.target.value)} placeholder="Cari teman..." className="w-full text-xs bg-white border border-slate-300 rounded-lg px-2.5 py-1.5 focus:outline-none" />
                  </div>
                  {filteredFriends.filter(f=>f.status==='online').map((friend) => (
                    <div key={friend.id} onClick={() => handleOpenPMTab(friend.name)} className="p-2.5 px-3 flex items-center gap-3 hover:bg-cyan-50 cursor-pointer border-b border-slate-100 transition group">
                      <div className={`w-8 h-8 rounded-full ${friend.avatarBg} text-white flex items-center justify-center font-bold text-xs shrink-0 group-hover:scale-105 transition`}>
                        {friend.avatarIcon}
                      </div>
                      <div className="flex-1 min-w-0">
                        <span className="font-semibold text-xs text-slate-800">{friend.name}</span>
                        <p className="text-[10px] text-slate-500 truncate">{friend.mood}</p>
                      </div>
                      <span className="w-2.5 h-2.5 bg-emerald-500 rounded-full shrink-0"></span>
                    </div>
                  ))}
                </div>
              )}

              {activeTabId === 'rooms' && (
                <div className="divide-y divide-slate-100">
                  <div className="p-2.5 bg-slate-50 border-b border-slate-200 flex items-center gap-2">
                    <Search className="w-4 h-4 text-slate-400" />
                    <input type="text" value={searchRoomQuery} onChange={(e) => setSearchRoomQuery(e.target.value)} placeholder="Cari room..." className="w-full text-xs bg-white border border-slate-300 rounded-lg px-2.5 py-1.5 focus:outline-none" />
                  </div>
                  {chatRooms.filter(r => r.name.toLowerCase().includes(searchRoomQuery.toLowerCase())).map((room) => (
                    <div key={room.id} onClick={() => handleOpenRoomTab(room)} className="p-2.5 px-3 flex items-center justify-between hover:bg-cyan-50 cursor-pointer border-b border-slate-100 transition group">
                      <div className="flex items-center gap-2.5">
                        <div className="w-7 h-7 bg-amber-100 text-amber-700 rounded-lg flex items-center justify-center font-bold text-xs">💬</div>
                        <span className="font-semibold text-xs text-slate-800">#{room.name}</span>
                      </div>
                      <span className="text-[10px] text-slate-500 font-mono">({room.users}/{room.max})</span>
                    </div>
                  ))}
                </div>
              )}

              {activeTabId === 'games' && (
                <div className="p-3 space-y-3">
                  <div className="bg-gradient-to-r from-amber-500 to-orange-500 text-white p-2.5 rounded-xl shadow-sm text-xs">
                    <p className="font-bold">🎮 Browser Web Games Zone</p>
                  </div>
                  {!activeGame ? (
                    <div className="grid grid-cols-1 gap-2">
                      <div onClick={() => setActiveGame('dice')} className="bg-white border p-3 rounded-xl shadow-sm hover:border-orange-400 cursor-pointer flex justify-between items-center">
                        <span className="font-bold text-xs">🎲 Dice 10 Challenge</span>
                        <span className="text-orange-600 font-bold text-[10px]">Play →</span>
                      </div>
                    </div>
                  ) : (
                    <div>
                      <button onClick={() => setActiveGame(null)} className="mb-2 text-xs text-cyan-700 font-bold underline">‹ Kembali</button>
                      <div className="bg-white p-3 rounded-xl border text-center space-y-2">
                        <h4 className="font-bold text-xs">Dice 10</h4>
                        <div className="flex justify-around py-2 bg-slate-50 rounded font-bold text-sm">
                          <span>Kamu: {diceState.playerRoll ?? '?'}</span>
                          <span>Bot: {diceState.botRoll ?? '?'}</span>
                        </div>
                        <button onClick={handleRollDice} disabled={diceState.isPlaying} className="w-full py-2 bg-orange-500 text-white font-bold rounded-lg text-xs">Kocok Dadu</button>
                      </div>
                    </div>
                  )}
                </div>
              )}

              {activeTabId === 'updates' && (
                <div className="p-3 space-y-2 text-xs">
                  <p className="font-bold text-cyan-900">Feed Aktivitas Terbaru</p>
                  <div className="bg-slate-50 p-2.5 rounded-xl border">
                    <span className="font-bold text-cyan-700">@reason008</span>
                    <p className="text-slate-600">Lagi seru nih di room #sampit_terindah!</p>
                  </div>
                </div>
              )}

              {/* TAMPILAN JIKA TAB AKTIF ADALAH CHAT (ROOM ATAU PM) */}
              {activeTabObj?.type && (
                <div className="flex-1 flex flex-col h-full bg-white">
                  <div className="bg-slate-100 px-3 py-2 border-b border-slate-200 flex items-center justify-between text-xs">
                    <span className="font-bold text-slate-800">{activeTabObj.title} (Terhubung)</span>
                    <button onClick={handleSendEgg} className="bg-amber-500 hover:bg-amber-600 text-white font-bold px-2.5 py-1 rounded-lg text-[10px]">🥚 Lempar Telur</button>
                  </div>

                  <div className="flex-1 p-3 overflow-y-auto space-y-2 bg-[#fdfcfa]">
                    {(chatHistories[activeTabId] || []).map((msg) => (
                      <div key={msg.id} className="text-xs">
                        {msg.isSystem ? (
                          <p className="text-[10px] text-amber-700 bg-amber-50 p-1.5 rounded text-center italic">{msg.text}</p>
                        ) : (
                          <div>
                            <span className={`font-bold text-[11px] ${msg.sender === username ? 'text-orange-600' : 'text-cyan-800'}`}>{msg.sender}: </span>
                            <span className="text-slate-800 bg-slate-100 p-2 rounded-xl inline-block mt-0.5">{msg.text}</span>
                          </div>
                        )}
                      </div>
                    ))}
                    <div ref={chatBottomRef} />
                  </div>

                  <form onSubmit={handleSendMessage} className="p-2 bg-slate-200 border-t flex items-center gap-1.5">
                    <input type="text" value={chatInput} onChange={(e) => setChatInput(e.target.value)} placeholder="Ketik pesan..." className="flex-1 text-xs px-3 py-1.5 bg-white border rounded-lg focus:outline-none" />
                    <button type="submit" className="px-4 py-1.5 bg-cyan-700 text-white font-bold rounded-lg text-xs">Kirim</button>
                  </form>
                </div>
              )}
            </div>

          </div>
        )}

      </div>

    </div>
  );
}