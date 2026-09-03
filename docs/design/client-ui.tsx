import React, { useState, useEffect, useRef } from 'react';
import {
  User,
  Users,
  MessageSquare,
  Gift,
  Gamepad2,
  Bell,
  Mail,
  Settings,
  HelpCircle,
  LogOut,
  ChevronDown,
  ChevronRight,
  Search,
  RefreshCw,
  Volume2,
  VolumeX,
  Smartphone,
  Laptop,
  Send,
  Smile,
  Shield,
  Crown,
  Play,
  CheckSquare,
  Square,
  X,
  PlusCircle,
  Sparkles,
  Dices,
  Hash,
  AtSign,
  Eye,
  EyeOff,
  CheckCheck,
  Zap,
  Award,
  Flame,
  Heart,
  Sparkle,
  Trophy,
  RotateCcw,
  HelpCircle as QuizIcon,
  Layers,
  Star,
  UserPlus,
  Edit3,
  Key,
  Info,
  CreditCard,
  Lock,
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
    // Audio Context disekat otomatis jika belum ada interaksi
  }
};

export default function App() {
  const [soundEnabled, setSoundEnabled] = useState(true);

  // Status Auth & Form View Mode: 'login' | 'register'
  const [authView, setAuthView] = useState('login');
  const [isLoggedIn, setIsLoggedIn] = useState(false);

  // Login State
  const [username, setUsername] = useState('reason007007');
  const [passphrase, setPassphrase] = useState('••••••••');
  const [showPassphrase, setShowPassphrase] = useState(false);
  const [rememberMe, setRememberMe] = useState(true);
  const [loginInvisible, setLoginInvisible] = useState(false);

  // Register State
  const [regUsername, setRegUsername] = useState('');
  const [regEmail, setRegEmail] = useState('');
  const [regPassphrase, setRegPassphrase] = useState('');
  const [regConfirmPassphrase, setRegConfirmPassphrase] = useState('');

  // Avatar Dropdown Menu Modal & Sub-Modals
  const [showAvatarMenu, setShowAvatarMenu] = useState(false);
  const [activeModal, setActiveModal] = useState(null); // null | 'profile' | 'settings' | 'help' | 'topup'

  // Sistem Tab Utama & Dynamic Workspace
  const initialSystemTabs = [
    { id: 'friends', title: 'Friends', type: 'system', icon: Users },
    { id: 'rooms', title: 'Rooms', type: 'system', icon: MessageSquare },
    { id: 'games', title: 'Games', type: 'system', icon: Gamepad2 },
    { id: 'updates', title: 'Feed', type: 'system', icon: Sparkles },
  ];
  const [openTabs, setOpenTabs] = useState(initialSystemTabs);
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
  const [showPicker, setShowPicker] = useState(false);
  const [pickerTab, setPickerTab] = useState('emoji');

  // Popup Toast
  const [eggAnimation, setEggAnimation] = useState(null);

  // Data Teman
  const [friends, setFriends] = useState([
    {
      id: 1,
      name: 'reason008',
      status: 'online',
      isVip: true,
      mood: 'Main dice yuk!',
      avatarBg: 'bg-emerald-500',
      avatarIcon: '🤖',
    },
    {
      id: 2,
      name: 'nrock',
      status: 'online',
      isVip: false,
      mood: 'Listening to Linkin Park',
      avatarBg: 'bg-[#00BCD4]',
      avatarIcon: '🎧',
    },
    {
      id: 3,
      name: 'neel_the_great',
      status: 'online',
      isVip: true,
      mood: 'Salam kawan semua ✌️',
      avatarBg: 'bg-indigo-500',
      avatarIcon: '👑',
    },
    {
      id: 4,
      name: 'ahok',
      status: 'online',
      isVip: false,
      mood: 'Ada yang mau barter egg?',
      avatarBg: 'bg-amber-500',
      avatarIcon: '🥚',
    },
    {
      id: 5,
      name: 'sampit_gaul',
      status: 'offline',
      isVip: false,
      mood: 'Tidur dulu zzz...',
      avatarBg: 'bg-slate-400',
      avatarIcon: '😴',
    },
  ]);

  // Data Room Obrolan
  const [chatRooms, setChatRooms] = useState([
    {
      id: 'r1',
      name: 'sampit_terindah',
      users: 18,
      max: 30,
      category: 'Recent Rooms',
      badge: 'Popular',
    },
    {
      id: 'r2',
      name: 'indo_terindah',
      users: 24,
      max: 40,
      category: 'Recent Rooms',
      badge: 'Active',
    },
    {
      id: 'r3',
      name: 'malang_jomblo2',
      users: 12,
      max: 40,
      category: 'Recent Rooms',
      badge: 'Fun',
    },
    { id: 'r4', name: 'Jakarta_Gaul', users: 42, max: 50, category: 'Favorites', badge: 'Hot' },
    {
      id: 'r5',
      name: 'Cari_Jodoh_Nusantara',
      users: 38,
      max: 50,
      category: 'Favorites',
      badge: 'Top',
    },
  ]);

  // --- GAME CENTER STATES ---
  const [activeGame, setActiveGame] = useState(null); // null (dashboard) | 'dice' | 'lowcard' | 'quiz' | 'spin'

  // Game 1: Dice 10
  const [diceState, setDiceState] = useState({
    playerRoll: null,
    botRoll: null,
    result: null,
    bet: 500,
    isPlaying: false,
    wins: 3,
    losses: 1,
  });

  // Game 2: Low Card 7
  const [lowCardState, setLowCardState] = useState({
    playerCard: null,
    botCard: null,
    result: null,
    bet: 500,
    isPlaying: false,
  });

  // Game 3: Questions 5 Quiz
  const [quizState, setQuizState] = useState({
    currentQuestion: 0,
    score: 0,
    selectedOption: null,
    isFinished: false,
    rewardClaimed: false,
  });

  const quizQuestions = [
    {
      q: 'Apa sebutan untuk melempar item spesial di room obrolan?',
      options: ['Lempar Telur', 'Lempar Batu', 'Kirim Bintang', 'Boom Chat'],
      answer: 0,
    },
    {
      q: 'Fitur apa yang memungkinkan Anda mengobrol tanpa terlihat online?',
      options: ['Ghost Mode', 'Invisible Login', 'Incognito Chat', 'Offline Status'],
      answer: 1,
    },
    {
      q: 'Di game Low Card 7, pemenang ditentukan berdasarkan?',
      options: ['Kartu Tertinggi', 'Kartu Terendah', 'Warna Kartu', 'Kartu Kembar'],
      answer: 1,
    },
    {
      q: 'Apa mata uang yang digunakan di simulator ini?',
      options: ['Gold Coin', 'migCredit', 'Rupiah / Credits', 'Diamonds'],
      answer: 2,
    },
    {
      q: 'Berapa angka maksimal pemain di room favorit "Jakarta_Gaul"?',
      options: ['30', '40', '50', '100'],
      answer: 2,
    },
  ];

  // Game 4: Lucky Wheel
  const [wheelState, setWheelState] = useState({
    isSpinning: false,
    lastReward: null,
  });

  // Emotikon & Stiker
  const emoticons = [
    { code: ':D', symbol: '😃', category: 'emoji' },
    { code: ';P', symbol: '😜', category: 'emoji' },
    { code: '❤️', symbol: '❤️', category: 'emoji' },
    { code: '🔥', symbol: '🔥', category: 'emoji' },
    { code: '👍', symbol: '👍', category: 'emoji' },
    { code: '(bot)', symbol: '🤖', category: 'sticker' },
    { code: '(crown)', symbol: '👑', category: 'sticker' },
    { code: '(egg)', symbol: '🥚', category: 'gift' },
    { code: '(gift)', symbol: '🎁', category: 'gift' },
  ];

  const chatBottomRef = useRef(null);

  // Auto Scroll Chat
  useEffect(() => {
    chatBottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [chatHistories, activeTabId]);

  const triggerFx = (type) => {
    if (soundEnabled) playSound(type);
  };

  // Login
  const handleLogin = (e) => {
    if (e) e.preventDefault();
    triggerFx('click');
    setIsLoggedIn(true);
    if (loginInvisible) setUserStatus('Invisible');
  };

  // Register Handler
  const handleRegister = (e) => {
    e.preventDefault();
    if (regPassphrase !== regConfirmPassphrase) {
      alert('Passphrase dan Konfirmasi Passphrase tidak cocok!');
      return;
    }
    triggerFx('click');
    setUsername(regUsername || 'user_baru');
    setIsLoggedIn(true);
    setAuthView('login');
  };

  // Logout
  const handleLogout = () => {
    triggerFx('click');
    setIsLoggedIn(false);
    setShowAvatarMenu(false);
    setActiveModal(null);
    setOpenTabs(initialSystemTabs);
    setActiveTabId('friends');
    setActiveGame(null);
  };

  // Buka Room Obrolan di Tab Baru
  const handleOpenRoomTab = (room) => {
    triggerFx('click');
    const tabId = `room-${room.id}`;

    const exists = openTabs.find((t) => t.id === tabId);
    if (!exists) {
      const newTab = {
        id: tabId,
        title: `#${room.name}`,
        type: 'chatroom',
        roomData: room,
        closable: true,
      };
      setOpenTabs((prev) => [...prev, newTab]);

      if (!chatHistories[tabId]) {
        setChatHistories((prev) => ({
          ...prev,
          [tabId]: [
            {
              id: 1,
              sender: 'System',
              text: `*** Selamat datang di room #${room.name} ***`,
              isSystem: true,
            },
            {
              id: 2,
              sender: 'Bot',
              text: 'Halo gaes! Jaga kesopanan & patuhi rule room ya.',
              isBot: true,
            },
            {
              id: 3,
              sender: 'reason008',
              text: `Halo @${username}! Selamat bergabung bro 🎉`,
              time: '11:32',
            },
            {
              id: 4,
              sender: 'sampit_gaul',
              text: 'Ada yang mau mabar game dadu hari ini?',
              time: '11:33',
            },
          ],
        }));
      }
    }
    setActiveTabId(tabId);
  };

  // Buka PM di Tab Baru
  const handleOpenPMTab = (friendName) => {
    triggerFx('click');
    const tabId = `pm-${friendName}`;

    const exists = openTabs.find((t) => t.id === tabId);
    if (!exists) {
      const newTab = {
        id: tabId,
        title: `@${friendName}`,
        type: 'pm',
        targetName: friendName,
        closable: true,
      };
      setOpenTabs((prev) => [...prev, newTab]);

      if (!chatHistories[tabId]) {
        setChatHistories((prev) => ({
          ...prev,
          [tabId]: [
            {
              id: 1,
              sender: 'System',
              text: `Sesi obrolan pribadi bersama ${friendName}`,
              isSystem: true,
            },
            { id: 2, sender: friendName, text: 'Oi bro, lagi di mana?', time: '11:30' },
          ],
        }));
      }
    }
    setActiveTabId(tabId);
  };

  // Tutup Tab
  const handleCloseTab = (e, tabIdToClose) => {
    e.stopPropagation();
    triggerFx('click');

    const nextTabs = openTabs.filter((t) => t.id !== tabIdToClose);
    setOpenTabs(nextTabs);

    if (activeTabId === tabIdToClose) {
      setActiveTabId(nextTabs[nextTabs.length - 1]?.id || 'friends');
    }
  };

  // Kirim Pesan di Tab Aktif
  const handleSendMessage = (e) => {
    e.preventDefault();
    if (!chatInput.trim()) return;
    triggerFx('click');

    const activeTab = openTabs.find((t) => t.id === activeTabId);
    if (!activeTab) return;

    const newMsg = {
      id: Date.now(),
      sender: username,
      text: chatInput,
      time: new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }),
    };

    setChatHistories((prev) => ({
      ...prev,
      [activeTabId]: [...(prev[activeTabId] || []), newMsg],
    }));
    setChatInput('');
    setShowPicker(false);

    // Simulasi Jawaban Otomatis
    setTimeout(() => {
      triggerFx('msg');
      let replyMsg;
      if (activeTab.type === 'chatroom') {
        replyMsg = {
          id: Date.now() + 1,
          sender: 'reason008',
          text: `Mantap Bro! 🥚 ${chatInput.includes(':D') ? '😀' : ''}`,
          time: new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }),
        };
      } else if (activeTab.type === 'pm') {
        replyMsg = {
          id: Date.now() + 1,
          sender: activeTab.targetName,
          text: 'Sip mas, nanti aku kirim gift telur ya!',
          time: new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }),
        };
      }
      if (replyMsg) {
        setChatHistories((prev) => ({
          ...prev,
          [activeTabId]: [...(prev[activeTabId] || []), replyMsg],
        }));
      }
    }, 1200);
  };

  // Fitur Lempar Telur
  const handleSendEgg = () => {
    if (eggCount <= 0) return;
    triggerFx('egg');
    setEggCount((prev) => prev - 1);
    setUserXp((prev) => Math.min(100, prev + 10));

    const activeTab = openTabs.find((t) => t.id === activeTabId);
    const targetText = activeTab?.title || 'Room';

    setEggAnimation(`🥚 Melempar Telur ke ${targetText}! (+10 EXP)`);
    setTimeout(() => setEggAnimation(null), 2000);

    setChatHistories((prev) => ({
      ...prev,
      [activeTabId]: [
        ...(prev[activeTabId] || []),
        {
          id: Date.now(),
          sender: 'System',
          text: `🥚 ${username} melempar TELUR KELUARGA ke ${targetText}! (+10 Exp)`,
          isSystem: true,
        },
      ],
    }));
  };

  // LOGIKA GAME 1: Dice 10
  const handleRollDice = () => {
    if (credits < diceState.bet) {
      alert('Kredit Anda tidak mencukupi! Silakan lakukan top up terlebih dahulu.');
      return;
    }
    triggerFx('dice');
    setDiceState((prev) => ({ ...prev, isPlaying: true }));

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

      setCredits((prev) => prev + change);
      setDiceState({
        playerRoll: pRoll,
        botRoll: bRoll,
        result: res,
        bet: diceState.bet,
        isPlaying: false,
        wins: newWins,
        losses: newLosses,
      });
    }, 700);
  };

  // LOGIKA GAME 2: Low Card 7
  const handlePlayLowCard = () => {
    if (credits < lowCardState.bet) {
      alert('Kredit tidak cukup!');
      return;
    }
    triggerFx('dice');
    setLowCardState((prev) => ({ ...prev, isPlaying: true }));

    setTimeout(() => {
      const pCard = Math.floor(Math.random() * 10) + 1;
      const bCard = Math.floor(Math.random() * 10) + 1;
      let res = 'SERI';
      let change = 0;

      if (pCard < bCard) {
        res = 'MENANG';
        change = lowCardState.bet;
        triggerFx('win');
      } else if (pCard > bCard) {
        res = 'KALAH';
        change = -lowCardState.bet;
        triggerFx('msg');
      }

      setCredits((prev) => prev + change);
      setLowCardState((prev) => ({
        ...prev,
        playerCard: pCard,
        botCard: bCard,
        result: res,
        isPlaying: false,
      }));
    }, 600);
  };

  // LOGIKA GAME 3: Questions 5 Quiz
  const handleAnswerQuiz = (optionIdx) => {
    triggerFx('click');
    setQuizState((prev) => ({ ...prev, selectedOption: optionIdx }));

    setTimeout(() => {
      const isCorrect = optionIdx === quizQuestions[quizState.currentQuestion].answer;
      if (isCorrect) {
        triggerFx('win');
      }

      if (quizState.currentQuestion + 1 < quizQuestions.length) {
        setQuizState((prev) => ({
          ...prev,
          score: isCorrect ? prev.score + 20 : prev.score,
          currentQuestion: prev.currentQuestion + 1,
          selectedOption: null,
        }));
      } else {
        const finalScore = isCorrect ? quizState.score + 20 : quizState.score;
        const reward = finalScore * 20;
        setCredits((prev) => prev + reward);
        setQuizState((prev) => ({
          ...prev,
          score: finalScore,
          isFinished: true,
          selectedOption: null,
          rewardClaimed: true,
        }));
      }
    }, 600);
  };

  const handleResetQuiz = () => {
    triggerFx('click');
    setQuizState({
      currentQuestion: 0,
      score: 0,
      selectedOption: null,
      isFinished: false,
      rewardClaimed: false,
    });
  };

  // LOGIKA GAME 4: Lucky Spin Wheel
  const handleSpinWheel = () => {
    if (wheelState.isSpinning) return;
    triggerFx('dice');
    setWheelState({ isSpinning: true, lastReward: null });

    setTimeout(() => {
      const rewards = [
        { text: '+500 Credits', credits: 500, eggs: 0 },
        { text: '+2 Telur 🥚', credits: 0, eggs: 2 },
        { text: '+1000 Credits 🎉', credits: 1000, eggs: 0 },
        { text: '+5 Telur 🥚✨', credits: 0, eggs: 5 },
        { text: '+2500 Credits Jackpot! 💎', credits: 2500, eggs: 1 },
      ];

      const won = rewards[Math.floor(Math.random() * rewards.length)];
      setCredits((prev) => prev + won.credits);
      setEggCount((prev) => prev + won.eggs);
      triggerFx('win');

      setWheelState({
        isSpinning: false,
        lastReward: won.text,
      });
    }, 1200);
  };

  const activeTabObject = openTabs.find((t) => t.id === activeTabId) || openTabs[0];

  const filteredFriends = friends.filter(
    (f) =>
      f.name.toLowerCase().includes(searchFriendQuery.toLowerCase()) ||
      f.mood.toLowerCase().includes(searchFriendQuery.toLowerCase()),
  );

  return (
    <div className="min-h-screen bg-slate-950 text-slate-100 flex flex-col items-center justify-center p-3 sm:p-6 font-sans select-none antialiased">
      {/* CONTAINER UTAMA APLIKASI (FOKUS UI UTAMA TANPA HEADER/FOOTER LUAR) */}
      <div className="w-full max-w-2xl bg-[#fdfbf7] text-slate-900 rounded-3xl border-2 border-slate-700 shadow-2xl flex flex-col min-h-[660px] max-h-[800px] overflow-hidden relative font-sans text-xs">
        {!isLoggedIn ? (
          /* HOMEPAGE AUTH (LOGIN / REGISTER FORM) */
          <div className="flex-1 bg-gradient-to-b from-[#0093AF] via-[#00ACC1] to-[#00838F] flex flex-col items-center justify-between p-6 sm:p-8 text-white relative overflow-hidden">
            <div className="w-full flex justify-between items-center text-[11px] text-cyan-100 font-mono mb-2 z-10">
              <span className="flex items-center gap-1.5">
                <span className="w-2.5 h-2.5 rounded-full bg-emerald-400 animate-ping"></span>
                TELKOMSEL 3G
              </span>
              <span>11:33 🔋</span>
            </div>

            <div className="flex flex-col items-center my-auto z-10 w-full max-w-xs space-y-4">
              <div className="bg-[#00BCD4] border-2 border-white text-white px-8 py-3.5 rounded-3xl font-extrabold text-3xl shadow-xl tracking-wider flex items-center gap-2">
                <MessageSquare className="w-8 h-8 text-white" />
                <span>Chat</span>
              </div>

              <div className="flex items-end justify-center gap-2">
                <div className="w-8 h-8 bg-emerald-400 border-2 border-white rounded-t-full flex items-center justify-center text-sm shadow-md">
                  🤖
                </div>
                <div className="w-10 h-10 bg-slate-100 border-2 border-slate-300 rounded-t-full flex items-center justify-center text-base shadow-lg">
                  🤖
                </div>
                <div className="w-8 h-8 bg-pink-400 border-2 border-white rounded-t-full flex items-center justify-center text-sm shadow-md">
                  🌸
                </div>
              </div>

              <p className="text-xs font-semibold text-cyan-100 tracking-wide">
                {authView === 'login' ? 'Join the Fun!' : 'Buat Akun Baru Sekarang'}
              </p>

              {/* FORM LOGIN */}
              {authView === 'login' ? (
                <form
                  onSubmit={handleLogin}
                  className="w-full space-y-3 bg-white/15 p-5 rounded-2xl border border-white/30 backdrop-blur-md shadow-2xl"
                >
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
                      type={showPassphrase ? 'text' : 'passphrase'}
                      value={passphrase}
                      onChange={(e) => setPassphrase(e.target.value)}
                      placeholder="Passphrase"
                      className="w-full px-3.5 py-2.5 text-slate-800 bg-white border border-cyan-200 rounded-xl shadow-inner focus:outline-none focus:ring-2 focus:ring-cyan-400 text-xs pr-9 transition"
                      required
                    />
                    <button
                      type="button"
                      onClick={() => setShowPassphrase(!showPassphrase)}
                      className="absolute right-3 top-3 text-slate-400 hover:text-cyan-700 transition"
                    >
                      {showPassphrase ? <EyeOff className="w-4 h-4" /> : <Eye className="w-4 h-4" />}
                    </button>
                  </div>

                  <button
                    type="submit"
                    className="w-full py-2.5 bg-gradient-to-r from-orange-500 to-amber-500 hover:from-orange-600 hover:to-amber-600 text-white font-bold rounded-xl shadow-lg border border-orange-300 active:scale-95 transition text-xs tracking-wider"
                  >
                    Go!
                  </button>

                  <div className="flex items-center justify-center gap-1.5 pt-1 text-[11px] text-cyan-50">
                    <input
                      type="checkbox"
                      id="invisibleCheck"
                      checked={loginInvisible}
                      onChange={(e) => setLoginInvisible(e.target.checked)}
                      className="rounded border-cyan-300 accent-orange-500 cursor-pointer"
                    />
                    <label htmlFor="invisibleCheck" className="cursor-pointer">
                      Login as Invisible
                    </label>
                  </div>
                </form>
              ) : (
                /* FORM CREATE ACCOUNT (FUNGSIONAL) */
                <form
                  onSubmit={handleRegister}
                  className="w-full space-y-2.5 bg-white/15 p-5 rounded-2xl border border-white/30 backdrop-blur-md shadow-2xl"
                >
                  <div>
                    <input
                      type="text"
                      value={regUsername}
                      onChange={(e) => setRegUsername(e.target.value)}
                      placeholder="Username Baru"
                      className="w-full px-3.5 py-2 text-slate-800 bg-white border border-cyan-200 rounded-xl shadow-inner focus:outline-none focus:ring-2 focus:ring-cyan-400 text-xs transition"
                      required
                    />
                  </div>
                  <div>
                    <input
                      type="email"
                      value={regEmail}
                      onChange={(e) => setRegEmail(e.target.value)}
                      placeholder="Alamat Email"
                      className="w-full px-3.5 py-2 text-slate-800 bg-white border border-cyan-200 rounded-xl shadow-inner focus:outline-none focus:ring-2 focus:ring-cyan-400 text-xs transition"
                      required
                    />
                  </div>
                  <div>
                    <input
                      type="passphrase"
                      value={regPassphrase}
                      onChange={(e) => setRegPassphrase(e.target.value)}
                      placeholder="Passphrase Baru"
                      className="w-full px-3.5 py-2 text-slate-800 bg-white border border-cyan-200 rounded-xl shadow-inner focus:outline-none focus:ring-2 focus:ring-cyan-400 text-xs transition"
                      required
                    />
                  </div>
                  <div>
                    <input
                      type="passphrase"
                      value={regConfirmPassphrase}
                      onChange={(e) => setRegConfirmPassphrase(e.target.value)}
                      placeholder="Konfirmasi Passphrase"
                      className="w-full px-3.5 py-2 text-slate-800 bg-white border border-cyan-200 rounded-xl shadow-inner focus:outline-none focus:ring-2 focus:ring-cyan-400 text-xs transition"
                      required
                    />
                  </div>

                  <button
                    type="submit"
                    className="w-full py-2.5 bg-gradient-to-r from-emerald-500 to-teal-600 hover:from-emerald-600 hover:to-teal-700 text-white font-bold rounded-xl shadow-lg border border-emerald-300 active:scale-95 transition text-xs tracking-wider"
                  >
                    Daftar Sekarang
                  </button>
                </form>
              )}
            </div>

            {/* SWITCHER LOGIN / REGISTER */}
            <div className="z-10 pb-2 text-center">
              {authView === 'login' ? (
                <button
                  onClick={() => {
                    triggerFx('click');
                    setAuthView('register');
                  }}
                  className="text-cyan-100 text-xs underline font-semibold hover:text-white"
                >
                  Create Account
                </button>
              ) : (
                <button
                  onClick={() => {
                    triggerFx('click');
                    setAuthView('login');
                  }}
                  className="text-cyan-100 text-xs underline font-semibold hover:text-white"
                >
                  Sudah Memiliki Akun? Login
                </button>
              )}
            </div>
          </div>
        ) : (
          /* WORKSPACE UTAMA SETELAH LOGIN */
          <div className="flex-1 flex flex-col bg-[#fdfbf7] text-slate-900 overflow-hidden relative">
            {/* Animasi Lempar Telur Popup Toast */}
            {eggAnimation && (
              <div className="absolute top-16 left-1/2 -translate-x-1/2 z-50 bg-amber-500 text-white px-4 py-2 rounded-2xl shadow-2xl font-bold text-xs flex items-center gap-2 border-2 border-white animate-bounce">
                <span>{eggAnimation}</span>
              </div>
            )}

            {/* TAB BAR CYAN KLASIK (#00838F) DENGAN FLOATING PILLS MODERN */}
            <div className="bg-[#00838F] text-white flex items-center text-[11px] font-semibold border-b border-cyan-900 shadow-sm overflow-x-auto no-scrollbar scroll-smooth p-1.5 gap-1.5">
              {openTabs.map((tab) => {
                const isActive = activeTabId === tab.id;
                const IconComponent = tab.icon;
                return (
                  <div
                    key={tab.id}
                    onClick={() => {
                      triggerFx('click');
                      setActiveTabId(tab.id);
                    }}
                    className={`py-1.5 px-3 rounded-xl flex items-center gap-1.5 cursor-pointer whitespace-nowrap transition-all duration-150 shrink-0 ${
                      isActive
                        ? 'bg-[#00ACC1] text-white font-bold shadow-md border-b-2 border-orange-400'
                        : 'bg-cyan-900/40 text-cyan-100 hover:bg-cyan-800/80'
                    }`}
                  >
                    {IconComponent && <IconComponent className="w-3.5 h-3.5 text-cyan-200" />}
                    {tab.type === 'chatroom' && (
                      <span className="text-amber-300 font-bold">💬</span>
                    )}
                    {tab.type === 'pm' && <span className="text-emerald-300 font-bold">👤</span>}

                    <span>{tab.title}</span>

                    {/* Tombol Tutup Tab */}
                    {tab.closable && (
                      <button
                        onClick={(e) => handleCloseTab(e, tab.id)}
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

            {/* BANNER PROFIL ORANGE IKONIK - AVATAR BISA DIKLIK BUKA POPOVER MENU LENGKAP */}
            <div className="bg-gradient-to-r from-orange-600 via-orange-500 to-amber-500 text-white p-2.5 px-3.5 flex items-center gap-3 border-b border-orange-600 shadow-inner relative">
              {/* AVATAR INTERAKSI DENGAN POPOVER MENU */}
              <div className="relative">
                <button
                  onClick={() => {
                    triggerFx('click');
                    setShowAvatarMenu(!showAvatarMenu);
                  }}
                  className="w-10 h-10 bg-white rounded-xl border-2 border-orange-200 p-0.5 flex items-center justify-center shadow hover:scale-105 active:scale-95 transition cursor-pointer"
                  title="Klik untuk Menu Profil & Pengaturan"
                >
                  <div className="w-full h-full bg-cyan-50 border border-cyan-200 rounded-lg flex items-center justify-center text-lg">
                    🤖
                  </div>
                </button>

                {/* DROPDOWN AVATAR MENU LENGKAP */}
                {showAvatarMenu && (
                  <div className="absolute top-12 left-0 w-56 bg-white text-slate-800 rounded-2xl shadow-2xl border border-slate-200 p-1.5 z-50 animate-in fade-in zoom-in-95">
                    <div className="p-2.5 bg-slate-50 border-b border-slate-100 rounded-xl mb-1">
                      <p className="font-bold text-xs text-slate-800">{username}</p>
                      <p className="text-[10px] text-slate-500">Status: {userStatus}</p>
                    </div>

                    <button
                      onClick={() => {
                        triggerFx('click');
                        setActiveModal('profile');
                        setShowAvatarMenu(false);
                      }}
                      className="w-full text-left px-3 py-2 text-xs font-semibold text-slate-700 hover:bg-cyan-50 hover:text-cyan-800 rounded-xl flex items-center gap-2 transition"
                    >
                      <User className="w-4 h-4 text-cyan-600" />
                      <span>My Profile</span>
                    </button>

                    <button
                      onClick={() => {
                        triggerFx('click');
                        setActiveModal('topup');
                        setShowAvatarMenu(false);
                      }}
                      className="w-full text-left px-3 py-2 text-xs font-semibold text-slate-700 hover:bg-cyan-50 hover:text-cyan-800 rounded-xl flex items-center gap-2 transition"
                    >
                      <CreditCard className="w-4 h-4 text-amber-600" />
                      <span>My Credits & TopUp</span>
                    </button>

                    <button
                      onClick={() => {
                        triggerFx('click');
                        setActiveModal('settings');
                        setShowAvatarMenu(false);
                      }}
                      className="w-full text-left px-3 py-2 text-xs font-semibold text-slate-700 hover:bg-cyan-50 hover:text-cyan-800 rounded-xl flex items-center gap-2 transition"
                    >
                      <Settings className="w-4 h-4 text-slate-600" />
                      <span>Settings</span>
                    </button>

                    <button
                      onClick={() => {
                        triggerFx('click');
                        setActiveModal('help');
                        setShowAvatarMenu(false);
                      }}
                      className="w-full text-left px-3 py-2 text-xs font-semibold text-slate-700 hover:bg-cyan-50 hover:text-cyan-800 rounded-xl flex items-center gap-2 transition"
                    >
                      <HelpCircle className="w-4 h-4 text-blue-600" />
                      <span>Help & Support</span>
                    </button>

                    <div className="border-t border-slate-100 my-1"></div>

                    <button
                      onClick={handleLogout}
                      className="w-full text-left px-3 py-2 text-xs font-bold text-rose-600 hover:bg-rose-50 rounded-xl flex items-center gap-2 transition"
                    >
                      <LogOut className="w-4 h-4 text-rose-600" />
                      <span>Exit / Logout</span>
                    </button>
                  </div>
                )}
              </div>

              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="w-2.5 h-2.5 bg-emerald-400 rounded-full border border-emerald-200 shadow-sm inline-block"></span>
                  <span className="font-bold text-xs truncate leading-none">{username}</span>
                  <span className="text-[9px] bg-orange-700/60 px-1.5 py-0.2 rounded font-mono font-bold text-amber-200">
                    Lvl {userLevel}
                  </span>
                </div>

                {/* Status & Progress Bar */}
                <p className="text-[10px] text-orange-100 truncate italic mt-0.5">{statusText}</p>
                <div className="w-full bg-orange-800/40 h-1 rounded-full mt-1 overflow-hidden">
                  <div
                    className="bg-amber-300 h-full rounded-full transition-all duration-300"
                    style={{ width: `${userXp}%` }}
                  ></div>
                </div>
              </div>

              <div className="bg-orange-700/40 border border-orange-300/40 rounded-xl px-2.5 py-1 text-center shrink-0 flex items-center gap-2.5">
                <div className="flex items-center gap-1 text-[11px] font-bold text-amber-200">
                  <span>🥚</span>
                  <span>{eggCount}</span>
                </div>
                <div className="h-3 w-px bg-orange-400/50"></div>
                <div className="text-[11px] font-bold text-emerald-200">
                  Rp {credits.toLocaleString()}
                </div>
              </div>
            </div>

            {/* MODAL WINDOWS INTERAKSI AVATAR */}
            {activeModal && (
              <div className="absolute inset-0 bg-slate-900/60 backdrop-blur-sm z-50 flex items-center justify-center p-4">
                <div className="bg-white rounded-2xl border border-slate-200 shadow-2xl w-full max-w-sm overflow-hidden animate-in zoom-in-95">
                  <div className="bg-slate-100 px-4 py-3 border-b border-slate-200 flex justify-between items-center font-bold text-xs text-slate-800">
                    <span>
                      {activeModal === 'profile' && '👤 My Profile'}
                      {activeModal === 'topup' && '💳 My Credits & Top Up'}
                      {activeModal === 'settings' && '⚙️ Settings'}
                      {activeModal === 'help' && '❓ Help & Support'}
                    </span>
                    <button
                      onClick={() => setActiveModal(null)}
                      className="p-1 hover:bg-slate-200 rounded-full"
                    >
                      <X className="w-4 h-4 text-slate-600" />
                    </button>
                  </div>

                  <div className="p-4 space-y-3 text-xs text-slate-700">
                    {activeModal === 'profile' && (
                      <div className="space-y-3">
                        <div className="flex items-center gap-3 bg-slate-50 p-3 rounded-xl border border-slate-200">
                          <div className="w-12 h-12 bg-cyan-100 rounded-xl flex items-center justify-center text-2xl border border-cyan-300">
                            🤖
                          </div>
                          <div>
                            <p className="font-bold text-sm text-slate-800">{username}</p>
                            <p className="text-[10px] text-slate-500">
                              Level {userLevel} Exp ({userXp}/100)
                            </p>
                          </div>
                        </div>

                        <div>
                          <label className="block text-[10px] font-bold text-slate-600 mb-1">
                            Update Status Mood:
                          </label>
                          <input
                            type="text"
                            value={statusText}
                            onChange={(e) => setStatusText(e.target.value)}
                            className="w-full px-3 py-2 border border-slate-300 rounded-xl focus:outline-none focus:border-cyan-600"
                          />
                        </div>

                        <div>
                          <label className="block text-[10px] font-bold text-slate-600 mb-1">
                            Status Kehadiran:
                          </label>
                          <select
                            value={userStatus}
                            onChange={(e) => setUserStatus(e.target.value)}
                            className="w-full px-3 py-2 border border-slate-300 rounded-xl focus:outline-none focus:border-cyan-600 bg-white"
                          >
                            <option value="Available">🟢 Available</option>
                            <option value="Away">🟡 Away</option>
                            <option value="Busy">🔴 Busy</option>
                            <option value="Invisible">⚪ Invisible</option>
                          </select>
                        </div>
                      </div>
                    )}

                    {activeModal === 'topup' && (
                      <div className="space-y-3">
                        <div className="bg-amber-50 p-3 rounded-xl border border-amber-200 text-amber-900">
                          <p className="font-bold text-xs">Saldo Kredit Anda:</p>
                          <p className="text-lg font-black text-amber-700 mt-1">
                            Rp {credits.toLocaleString()}
                          </p>
                        </div>

                        <p className="font-bold text-slate-800">Isi Saldo Instant Gratis:</p>
                        <div className="grid grid-cols-2 gap-2">
                          {[5000, 10000, 25000, 50000].map((amt) => (
                            <button
                              key={amt}
                              onClick={() => {
                                setCredits((prev) => prev + amt);
                                triggerFx('win');
                                alert(`Top up Rp ${amt.toLocaleString()} berhasil!`);
                              }}
                              className="p-2.5 bg-slate-50 border border-slate-200 hover:bg-cyan-50 hover:border-cyan-400 rounded-xl font-bold text-slate-800 text-xs transition"
                            >
                              + Rp {amt.toLocaleString()}
                            </button>
                          ))}
                        </div>
                      </div>
                    )}

                    {activeModal === 'settings' && (
                      <div className="space-y-2">
                        <label className="flex items-center justify-between p-2.5 bg-slate-50 border border-slate-200 rounded-xl cursor-pointer">
                          <span>Efek Suara Audio Retro</span>
                          <input
                            type="checkbox"
                            checked={soundEnabled}
                            onChange={(e) => setSoundEnabled(e.target.checked)}
                            className="accent-cyan-600"
                          />
                        </label>
                        <label className="flex items-center justify-between p-2.5 bg-slate-50 border border-slate-200 rounded-xl cursor-pointer">
                          <span>Ingat Passphrase Login</span>
                          <input
                            type="checkbox"
                            checked={rememberMe}
                            onChange={(e) => setRememberMe(e.target.checked)}
                            className="accent-cyan-600"
                          />
                        </label>
                      </div>
                    )}

                    {activeModal === 'help' && (
                      <div className="space-y-2 text-slate-600">
                        <p className="font-bold text-slate-800">Petunjuk Penggunaan Chat:</p>
                        <p>1. Klik nama kawan atau room obrolan untuk membuka tab obrolan baru.</p>
                        <p>2. Gunakan tombol "Lempar Telur" di ruang chat untuk berbagi exp.</p>
                        <p>3. Dapatkan saldo kredit dari permainan mini game di tab Games.</p>
                      </div>
                    )}
                  </div>

                  <div className="p-3 bg-slate-50 border-t border-slate-200 text-right">
                    <button
                      onClick={() => setActiveModal(null)}
                      className="px-4 py-1.5 bg-cyan-700 hover:bg-cyan-800 text-white font-bold rounded-xl text-xs"
                    >
                      Selesai
                    </button>
                  </div>
                </div>
              </div>
            )}

            {/* AREA KONTEN UTAMA */}
            <div className="flex-1 overflow-y-auto bg-white flex flex-col">
              {/* 1. FRIENDS TAB */}
              {activeTabId === 'friends' && (
                <div className="divide-y divide-slate-100">
                  {/* Search Bar Teman */}
                  <div className="p-2.5 bg-slate-50 border-b border-slate-200 flex items-center gap-2">
                    <Search className="w-4 h-4 text-slate-400" />
                    <input
                      type="text"
                      value={searchFriendQuery}
                      onChange={(e) => setSearchFriendQuery(e.target.value)}
                      placeholder="Cari teman online..."
                      className="w-full text-xs bg-white border border-slate-300 rounded-lg px-2.5 py-1.5 focus:outline-none focus:border-cyan-500"
                    />
                  </div>

                  {/* Messages Inboxes */}
                  <div className="p-2.5 flex items-center gap-2.5 bg-white hover:bg-slate-50 cursor-pointer transition">
                    <Mail className="w-4 h-4 text-cyan-600" />
                    <span className="text-xs font-medium text-slate-800">Messages</span>
                    <span className="text-xs text-slate-500 font-mono">(0)</span>
                  </div>

                  {/* Updates Notification */}
                  <div className="p-2.5 flex items-center gap-2.5 bg-white hover:bg-slate-50 cursor-pointer transition">
                    <div className="w-4 h-4 bg-orange-500 text-white rounded font-bold text-[10px] flex items-center justify-center shadow-sm">
                      !
                    </div>
                    <span className="text-xs font-bold text-orange-600">Updates</span>
                    <span className="text-xs font-bold text-orange-600 font-mono">(9)</span>
                  </div>

                  <div className="bg-slate-100 px-3 py-1.5 text-[10px] font-bold text-slate-500 uppercase tracking-wider flex justify-between items-center">
                    <span>
                      Teman Online ({filteredFriends.filter((f) => f.status === 'online').length})
                    </span>
                    <button
                      onClick={() => triggerFx('click')}
                      className="text-cyan-700 hover:underline"
                    >
                      Refresh
                    </button>
                  </div>

                  {/* Daftar Teman Online - KLIK BARIS ATAU AVATAR LANGSUNG BUKA PM */}
                  {filteredFriends
                    .filter((f) => f.status === 'online')
                    .map((friend) => (
                      <div
                        key={friend.id}
                        onClick={() => handleOpenPMTab(friend.name)}
                        className="p-2.5 px-3 flex items-center gap-3 hover:bg-cyan-50/70 cursor-pointer border-b border-slate-100 transition group"
                      >
                        {/* Avatar Logo Badge */}
                        <div
                          className={`w-8 h-8 rounded-full ${friend.avatarBg} text-white flex items-center justify-center font-bold shadow-md border-2 border-white text-xs shrink-0 group-hover:scale-105 transition`}
                        >
                          {friend.avatarIcon}
                        </div>

                        <div className="flex-1 min-w-0">
                          <div className="flex items-center gap-1.5">
                            <span className="font-semibold text-xs text-slate-800 group-hover:text-cyan-800">
                              {friend.name}
                            </span>
                            {friend.isVip && (
                              <Crown className="w-3.5 h-3.5 text-amber-500 fill-amber-400" />
                            )}
                          </div>
                          <p className="text-[10px] text-slate-500 truncate">{friend.mood}</p>
                        </div>

                        {/* Indikator Online Dot */}
                        <span className="w-2.5 h-2.5 bg-emerald-500 rounded-full border border-emerald-200 shrink-0"></span>
                      </div>
                    ))}

                  {/* SUB-BAR LAYANAN INSTANT MESSENGER (#E0F7FA KLASIK) */}
                  <div className="mt-2 space-y-0.5">
                    {['Facebook (0/0)', 'MSN (0/0)', 'Yahoo! (0/0)', 'GTalk (0/0)'].map(
                      (im, idx) => (
                        <div
                          key={idx}
                          className="bg-[#E0F7FA] border-y border-cyan-100 px-3 py-2 text-xs text-cyan-900 font-medium flex items-center justify-between hover:bg-cyan-100 cursor-pointer transition"
                        >
                          <div className="flex items-center gap-2">
                            <span className="font-bold text-cyan-700">–</span>
                            <span>{im}</span>
                          </div>
                          <ChevronRight className="w-3.5 h-3.5 text-cyan-600" />
                        </div>
                      ),
                    )}
                  </div>
                </div>
              )}

              {/* 2. CHAT ROOMS TAB - LANGSUNG KLIK MASUK */}
              {activeTabId === 'rooms' && (
                <div className="divide-y divide-slate-100">
                  <div className="p-2.5 bg-slate-50 border-b border-slate-200 flex items-center gap-2">
                    <Search className="w-4 h-4 text-slate-400" />
                    <input
                      type="text"
                      value={searchRoomQuery}
                      onChange={(e) => setSearchRoomQuery(e.target.value)}
                      placeholder="Cari ruang obrolan..."
                      className="w-full text-xs bg-white border border-slate-300 rounded-lg px-2.5 py-1.5 focus:outline-none focus:border-cyan-500"
                    />
                  </div>

                  <div className="bg-cyan-50/80 px-3 py-1.5 text-[10px] font-bold text-cyan-800 border-b border-cyan-100 flex items-center gap-1">
                    <ChevronDown className="w-3.5 h-3.5" />
                    <span>
                      Favorit ({chatRooms.filter((r) => r.category === 'Favorites').length})
                    </span>
                  </div>
                  {chatRooms
                    .filter(
                      (r) =>
                        r.category === 'Favorites' &&
                        r.name.toLowerCase().includes(searchRoomQuery.toLowerCase()),
                    )
                    .map((room) => (
                      <div
                        key={room.id}
                        onClick={() => handleOpenRoomTab(room)}
                        className="p-2.5 px-3 flex items-center justify-between hover:bg-cyan-50 cursor-pointer border-b border-slate-100 transition group"
                      >
                        <div className="flex items-center gap-2.5">
                          <div className="w-7 h-7 bg-amber-100 text-amber-700 rounded-lg flex items-center justify-center font-bold text-xs group-hover:bg-amber-500 group-hover:text-white transition">
                            💬
                          </div>
                          <div>
                            <span className="font-semibold text-xs text-slate-800 group-hover:text-cyan-800">
                              #{room.name}
                            </span>
                            <span className="ml-2 text-[9px] bg-amber-100 text-amber-800 px-1.5 py-0.2 rounded font-semibold">
                              {room.badge}
                            </span>
                          </div>
                        </div>
                        <div className="flex items-center gap-2">
                          <span className="text-[10px] text-slate-500 font-mono font-semibold">
                            ({room.users} / {room.max})
                          </span>
                          <ChevronRight className="w-4 h-4 text-slate-400 group-hover:text-cyan-600 transition" />
                        </div>
                      </div>
                    ))}

                  <div className="bg-cyan-50/80 px-3 py-1.5 text-[10px] font-bold text-cyan-800 border-b border-cyan-100 flex items-center gap-1">
                    <ChevronDown className="w-3.5 h-3.5" />
                    <span>
                      Recent Rooms ({chatRooms.filter((r) => r.category === 'Recent Rooms').length})
                    </span>
                  </div>
                  {chatRooms
                    .filter(
                      (r) =>
                        r.category === 'Recent Rooms' &&
                        r.name.toLowerCase().includes(searchRoomQuery.toLowerCase()),
                    )
                    .map((room) => (
                      <div
                        key={room.id}
                        onClick={() => handleOpenRoomTab(room)}
                        className="p-2.5 px-3 flex items-center justify-between hover:bg-cyan-50 cursor-pointer border-b border-slate-100 transition group"
                      >
                        <div className="flex items-center gap-2.5">
                          <div className="w-7 h-7 bg-cyan-100 text-cyan-700 rounded-lg flex items-center justify-center font-bold text-xs group-hover:bg-cyan-600 group-hover:text-white transition">
                            💬
                          </div>
                          <div>
                            <span className="font-semibold text-xs text-slate-800 group-hover:text-cyan-800">
                              #{room.name}
                            </span>
                            <span className="ml-2 text-[9px] bg-slate-100 text-slate-600 px-1.5 py-0.2 rounded font-semibold">
                              {room.badge}
                            </span>
                          </div>
                        </div>
                        <div className="flex items-center gap-2">
                          <span className="text-[10px] text-slate-500 font-mono font-semibold">
                            ({room.users} / {room.max})
                          </span>
                          <ChevronRight className="w-4 h-4 text-slate-400 group-hover:text-cyan-600 transition" />
                        </div>
                      </div>
                    ))}

                  <div className="p-3 text-center">
                    <button
                      onClick={() => alert('Fitur Buat Room Baru')}
                      className="text-xs text-cyan-700 font-bold hover:underline flex items-center justify-center gap-1 w-full py-2 bg-cyan-50 border border-cyan-200 rounded-xl transition"
                    >
                      <PlusCircle className="w-4 h-4" /> Create New Room
                    </button>
                  </div>
                </div>
              )}

              {/* 3. GAMES TAB - WEB BROWSER GAME PORTAL DASHBOARD + MINI GAMES */}
              {activeTabId === 'games' && (
                <div className="p-3 space-y-3">
                  {/* Dashboard Header Bar */}
                  <div className="bg-gradient-to-r from-amber-500 to-orange-500 text-white p-3 rounded-2xl shadow-sm text-xs flex justify-between items-center">
                    <div>
                      <p className="font-bold flex items-center gap-1">🎮 Browser Web Games Zone</p>
                      <p className="text-[10px] opacity-90">
                        Pilih & mainkan mini game langsung di browser!
                      </p>
                    </div>
                    {activeGame && (
                      <button
                        onClick={() => {
                          triggerFx('click');
                          setActiveGame(null);
                        }}
                        className="bg-white/20 hover:bg-white/30 text-white px-2.5 py-1 rounded-xl text-[10px] font-bold flex items-center gap-1 transition"
                      >
                        ‹ Back to Portal
                      </button>
                    )}
                  </div>

                  {/* PORTAL DASHBOARD VIEW (GRID GAMES) */}
                  {!activeGame ? (
                    <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                      {/* Game Card 1: Dice 10 */}
                      <div
                        onClick={() => {
                          triggerFx('click');
                          setActiveGame('dice');
                        }}
                        className="bg-white border border-slate-200 p-3.5 rounded-2xl shadow-sm hover:border-orange-400 hover:shadow-md cursor-pointer transition flex flex-col justify-between space-y-3 group"
                      >
                        <div className="flex items-start justify-between">
                          <div className="w-10 h-10 bg-gradient-to-br from-amber-400 to-orange-500 text-white rounded-xl flex items-center justify-center text-xl shadow group-hover:scale-105 transition">
                            🎲
                          </div>
                          <span className="text-[10px] bg-amber-100 text-amber-800 font-bold px-2 py-0.5 rounded-full">
                            ★ 4.9 (Hot)
                          </span>
                        </div>
                        <div>
                          <h4 className="font-bold text-xs text-slate-800">Dice 10 Challenge</h4>
                          <p className="text-[10px] text-slate-500 mt-0.5">
                            Kocok dadu melawan bot, kumpulkan kredit taruhan!
                          </p>
                        </div>
                        <div className="flex justify-between items-center text-[10px] text-slate-400 border-t border-slate-100 pt-2 font-mono">
                          <span>👥 1,240 Online</span>
                          <span className="text-orange-600 font-bold group-hover:underline">
                            Play Now →
                          </span>
                        </div>
                      </div>

                      {/* Game Card 2: Low Card 7 */}
                      <div
                        onClick={() => {
                          triggerFx('click');
                          setActiveGame('lowcard');
                        }}
                        className="bg-white border border-slate-200 p-3.5 rounded-2xl shadow-sm hover:border-cyan-400 hover:shadow-md cursor-pointer transition flex flex-col justify-between space-y-3 group"
                      >
                        <div className="flex items-start justify-between">
                          <div className="w-10 h-10 bg-gradient-to-br from-cyan-400 to-blue-500 text-white rounded-xl flex items-center justify-center text-xl shadow group-hover:scale-105 transition">
                            🃏
                          </div>
                          <span className="text-[10px] bg-cyan-100 text-cyan-800 font-bold px-2 py-0.5 rounded-full">
                            ★ 4.8 (Popular)
                          </span>
                        </div>
                        <div>
                          <h4 className="font-bold text-xs text-slate-800">Low Card 7</h4>
                          <p className="text-[10px] text-slate-500 mt-0.5">
                            Adu keberuntungan kartu terendah melawan Bot.
                          </p>
                        </div>
                        <div className="flex justify-between items-center text-[10px] text-slate-400 border-t border-slate-100 pt-2 font-mono">
                          <span>👥 850 Online</span>
                          <span className="text-cyan-600 font-bold group-hover:underline">
                            Play Now →
                          </span>
                        </div>
                      </div>

                      {/* Game Card 3: Questions 5 Trivia */}
                      <div
                        onClick={() => {
                          triggerFx('click');
                          setActiveGame('quiz');
                        }}
                        className="bg-white border border-slate-200 p-3.5 rounded-2xl shadow-sm hover:border-emerald-400 hover:shadow-md cursor-pointer transition flex flex-col justify-between space-y-3 group"
                      >
                        <div className="flex items-start justify-between">
                          <div className="w-10 h-10 bg-gradient-to-br from-emerald-400 to-teal-600 text-white rounded-xl flex items-center justify-center text-xl shadow group-hover:scale-105 transition">
                            ❓
                          </div>
                          <span className="text-[10px] bg-emerald-100 text-emerald-800 font-bold px-2 py-0.5 rounded-full">
                            ★ 4.7 (Quiz)
                          </span>
                        </div>
                        <div>
                          <h4 className="font-bold text-xs text-slate-800">Questions 5 Quiz</h4>
                          <p className="text-[10px] text-slate-500 mt-0.5">
                            Jawab 5 kuis pengetahuan umum & klaim bonus kredit!
                          </p>
                        </div>
                        <div className="flex justify-between items-center text-[10px] text-slate-400 border-t border-slate-100 pt-2 font-mono">
                          <span>👥 2,110 Online</span>
                          <span className="text-emerald-600 font-bold group-hover:underline">
                            Play Now →
                          </span>
                        </div>
                      </div>

                      {/* Game Card 4: Lucky Spin Wheel */}
                      <div
                        onClick={() => {
                          triggerFx('click');
                          setActiveGame('spin');
                        }}
                        className="bg-white border border-slate-200 p-3.5 rounded-2xl shadow-sm hover:border-purple-400 hover:shadow-md cursor-pointer transition flex flex-col justify-between space-y-3 group"
                      >
                        <div className="flex items-start justify-between">
                          <div className="w-10 h-10 bg-gradient-to-br from-purple-400 to-pink-500 text-white rounded-xl flex items-center justify-center text-xl shadow group-hover:scale-105 transition">
                            🎡
                          </div>
                          <span className="text-[10px] bg-purple-100 text-purple-800 font-bold px-2 py-0.5 rounded-full">
                            ★ 5.0 (Spin)
                          </span>
                        </div>
                        <div>
                          <h4 className="font-bold text-xs text-slate-800">Lucky Spin Wheel</h4>
                          <p className="text-[10px] text-slate-500 mt-0.5">
                            Putar Roda Keberuntungan Harian dapatkan Telur & Kredit!
                          </p>
                        </div>
                        <div className="flex justify-between items-center text-[10px] text-slate-400 border-t border-slate-100 pt-2 font-mono">
                          <span>👥 3,450 Online</span>
                          <span className="text-purple-600 font-bold group-hover:underline">
                            Play Now →
                          </span>
                        </div>
                      </div>
                    </div>
                  ) : (
                    /* PLAYABLE ACTIVE GAME VIEW */
                    <div>
                      {/* 1. DICE 10 GAME VIEW */}
                      {activeGame === 'dice' && (
                        <div className="bg-white p-4 rounded-2xl border border-slate-200 text-center shadow-sm space-y-3">
                          <h3 className="font-extrabold text-xs text-slate-800 flex items-center justify-center gap-1.5">
                            🎲 Game Dice 10
                          </h3>

                          <div className="flex justify-around items-center py-3 bg-slate-50 rounded-xl border border-slate-200">
                            <div className="flex flex-col items-center">
                              <span className="text-xs font-bold text-slate-600 mb-1">
                                {username}
                              </span>
                              <div className="w-13 h-13 bg-gradient-to-br from-amber-400 to-orange-500 rounded-xl flex items-center justify-center text-2xl text-white font-black shadow border-2 border-white">
                                {diceState.playerRoll !== null ? diceState.playerRoll : '?'}
                              </div>
                            </div>

                            <span className="font-black text-slate-400 text-xs">VS</span>

                            <div className="flex flex-col items-center">
                              <span className="text-xs font-bold text-slate-600 mb-1">Bot</span>
                              <div className="w-13 h-13 bg-gradient-to-br from-cyan-500 to-blue-600 rounded-xl flex items-center justify-center text-2xl text-white font-black shadow border-2 border-white">
                                {diceState.botRoll !== null ? diceState.botRoll : '?'}
                              </div>
                            </div>
                          </div>

                          {diceState.result && (
                            <div
                              className={`p-2 rounded-xl font-extrabold text-xs ${
                                diceState.result === 'MENANG'
                                  ? 'bg-emerald-100 text-emerald-800 border border-emerald-300'
                                  : diceState.result === 'KALAH'
                                    ? 'bg-rose-100 text-rose-800 border border-rose-300'
                                    : 'bg-slate-100 text-slate-800'
                              }`}
                            >
                              {diceState.result === 'MENANG' &&
                                `🎉 Menang +Rp ${diceState.bet.toLocaleString()}!`}
                              {diceState.result === 'KALAH' &&
                                `😭 Kalah -Rp ${diceState.bet.toLocaleString()}`}
                              {diceState.result === 'SERI' && '🤝 Seri!'}
                            </div>
                          )}

                          <div className="space-y-2">
                            <div className="flex items-center justify-center gap-2 text-xs">
                              <span className="text-slate-600">Taruhan:</span>
                              {[100, 500, 1000, 5000].map((amt) => (
                                <button
                                  key={amt}
                                  onClick={() => setDiceState((p) => ({ ...p, bet: amt }))}
                                  className={`px-2.5 py-1 rounded-lg text-xs font-bold transition ${
                                    diceState.bet === amt
                                      ? 'bg-orange-500 text-white shadow'
                                      : 'bg-slate-200 text-slate-700 hover:bg-slate-300'
                                  }`}
                                >
                                  {amt >= 1000 ? `${amt / 1000}k` : amt}
                                </button>
                              ))}
                            </div>

                            <button
                              onClick={handleRollDice}
                              disabled={diceState.isPlaying}
                              className="w-full py-2.5 bg-gradient-to-r from-orange-500 to-amber-500 text-white font-extrabold rounded-xl shadow border border-orange-300 active:scale-95 disabled:opacity-50 text-xs tracking-wider transition"
                            >
                              {diceState.isPlaying ? 'Mengocok Dadu...' : 'LEMPAR DADU!'}
                            </button>
                          </div>
                        </div>
                      )}

                      {/* 2. LOW CARD 7 GAME VIEW */}
                      {activeGame === 'lowcard' && (
                        <div className="bg-white p-4 rounded-2xl border border-slate-200 text-center shadow-sm space-y-3">
                          <h3 className="font-extrabold text-xs text-slate-800 flex items-center justify-center gap-1.5">
                            🃏 Low Card 7 (Kartu Terendah Menang)
                          </h3>

                          <div className="flex justify-around items-center py-4 bg-cyan-50/50 rounded-xl border border-cyan-100">
                            <div className="flex flex-col items-center">
                              <span className="text-xs font-bold text-slate-600 mb-1">
                                {username}
                              </span>
                              <div className="w-14 h-20 bg-gradient-to-br from-cyan-600 to-teal-700 rounded-xl flex flex-col items-center justify-center text-white font-black shadow-md border-2 border-white">
                                <span className="text-xl">
                                  {lowCardState.playerCard !== null ? lowCardState.playerCard : '?'}
                                </span>
                                <span className="text-[10px]">♠</span>
                              </div>
                            </div>

                            <span className="font-black text-slate-400 text-xs">VS</span>

                            <div className="flex flex-col items-center">
                              <span className="text-xs font-bold text-slate-600 mb-1">Bot</span>
                              <div className="w-14 h-20 bg-gradient-to-br from-slate-700 to-slate-900 rounded-xl flex flex-col items-center justify-center text-white font-black shadow-md border-2 border-white">
                                <span className="text-xl">
                                  {lowCardState.botCard !== null ? lowCardState.botCard : '?'}
                                </span>
                                <span className="text-[10px]">♦</span>
                              </div>
                            </div>
                          </div>

                          {lowCardState.result && (
                            <div
                              className={`p-2 rounded-xl font-extrabold text-xs ${
                                lowCardState.result === 'MENANG'
                                  ? 'bg-emerald-100 text-emerald-800 border border-emerald-300'
                                  : lowCardState.result === 'KALAH'
                                    ? 'bg-rose-100 text-rose-800 border border-rose-300'
                                    : 'bg-slate-100 text-slate-800'
                              }`}
                            >
                              {lowCardState.result === 'MENANG' &&
                                `🎉 Kartumu Lebih Rendah! Menang +Rp ${lowCardState.bet.toLocaleString()}!`}
                              {lowCardState.result === 'KALAH' &&
                                `😭 Kartumu Lebih Tinggi/Sama! Kalah -Rp ${lowCardState.bet.toLocaleString()}`}
                              {lowCardState.result === 'SERI' && '🤝 Nilai Kartu Sama (Seri)!'}
                            </div>
                          )}

                          <button
                            onClick={handlePlayLowCard}
                            disabled={lowCardState.isPlaying}
                            className="w-full py-2.5 bg-gradient-to-r from-cyan-600 to-teal-600 text-white font-extrabold rounded-xl shadow active:scale-95 disabled:opacity-50 text-xs tracking-wider transition"
                          >
                            {lowCardState.isPlaying ? 'Membuka Kartu...' : 'TARIK KARTU!'}
                          </button>
                        </div>
                      )}

                      {/* 3. QUESTIONS 5 QUIZ VIEW */}
                      {activeGame === 'quiz' && (
                        <div className="bg-white p-4 rounded-2xl border border-slate-200 shadow-sm space-y-3">
                          <div className="flex justify-between items-center border-b border-slate-100 pb-2">
                            <span className="font-bold text-xs text-slate-800">
                              Questions 5 Quiz ({quizState.currentQuestion + 1}/5)
                            </span>
                            <span className="text-[10px] bg-emerald-100 text-emerald-800 font-bold px-2 py-0.5 rounded">
                              Skor: {quizState.score}
                            </span>
                          </div>

                          {!quizState.isFinished ? (
                            <div className="space-y-3">
                              <p className="font-semibold text-xs text-slate-800 bg-slate-50 p-3 rounded-xl border border-slate-200">
                                {quizQuestions[quizState.currentQuestion].q}
                              </p>

                              <div className="space-y-2">
                                {quizQuestions[quizState.currentQuestion].options.map(
                                  (opt, oIdx) => (
                                    <button
                                      key={oIdx}
                                      onClick={() => handleAnswerQuiz(oIdx)}
                                      className="w-full text-left px-3.5 py-2.5 bg-white hover:bg-cyan-50 border border-slate-200 hover:border-cyan-400 rounded-xl font-medium text-xs text-slate-700 transition"
                                    >
                                      {String.fromCharCode(65 + oIdx)}. {opt}
                                    </button>
                                  ),
                                )}
                              </div>
                            </div>
                          ) : (
                            <div className="text-center py-4 space-y-3">
                              <Trophy className="w-10 h-10 text-amber-500 mx-auto" />
                              <h4 className="font-extrabold text-sm text-slate-800">
                                Kuis Selesai!
                              </h4>
                              <p className="text-xs text-slate-600">
                                Total Skor Kamu:{' '}
                                <span className="font-bold text-emerald-600">
                                  {quizState.score} / 100
                                </span>
                              </p>
                              <p className="text-[11px] bg-emerald-50 text-emerald-800 p-2 rounded-xl border border-emerald-200 font-semibold">
                                Hadiah Rp {(quizState.score * 20).toLocaleString()} telah
                                ditambahkan ke saldo kreditmu!
                              </p>
                              <button
                                onClick={handleResetQuiz}
                                className="px-4 py-2 bg-cyan-700 hover:bg-cyan-800 text-white font-bold rounded-xl text-xs shadow transition"
                              >
                                Main Lagi
                              </button>
                            </div>
                          )}
                        </div>
                      )}

                      {/* 4. LUCKY SPIN WHEEL VIEW */}
                      {activeGame === 'spin' && (
                        <div className="bg-white p-4 rounded-2xl border border-slate-200 text-center shadow-sm space-y-3">
                          <h3 className="font-extrabold text-xs text-slate-800 flex items-center justify-center gap-1.5">
                            🎡 Lucky Spin Wheel
                          </h3>

                          <div className="w-28 h-28 mx-auto rounded-full bg-gradient-to-tr from-purple-500 via-pink-500 to-amber-400 border-4 border-white shadow-lg flex items-center justify-center text-3xl font-black text-white relative animate-pulse">
                            🎰
                          </div>

                          {wheelState.lastReward && (
                            <div className="bg-purple-50 text-purple-800 p-2.5 rounded-xl border border-purple-200 font-extrabold text-xs">
                              Selamat! Anda Mendapatkan: {wheelState.lastReward}
                            </div>
                          )}

                          <button
                            onClick={handleSpinWheel}
                            disabled={wheelState.isSpinning}
                            className="w-full py-2.5 bg-gradient-to-r from-purple-600 to-pink-600 text-white font-extrabold rounded-xl shadow active:scale-95 disabled:opacity-50 text-xs tracking-wider transition"
                          >
                            {wheelState.isSpinning
                              ? 'Menumputar Roda...'
                              : 'PUTAR SEKARANG (GRATIS)'}
                          </button>
                        </div>
                      )}
                    </div>
                  )}
                </div>
              )}

              {/* 4. UPDATES / FEED TAB */}
              {activeTabId === 'updates' && (
                <div className="p-3 space-y-3">
                  <div className="bg-cyan-50 p-3 border border-cyan-200 rounded-xl text-xs space-y-2">
                    <p className="font-bold text-cyan-900">Update Status Kamu:</p>
                    <div className="flex gap-1.5">
                      <input
                        type="text"
                        value={statusText}
                        onChange={(e) => setStatusText(e.target.value)}
                        className="flex-1 text-xs border border-cyan-300 rounded-lg px-2.5 py-1.5 bg-white focus:outline-none"
                      />
                      <button
                        onClick={() => triggerFx('msg')}
                        className="bg-cyan-700 hover:bg-cyan-800 text-white px-3.5 py-1.5 rounded-lg font-bold text-[10px] transition"
                      >
                        Post
                      </button>
                    </div>
                  </div>

                  <div className="divide-y divide-slate-100 border border-slate-200 rounded-xl bg-white">
                    <div className="p-3 text-xs">
                      <div className="flex justify-between items-center font-bold text-slate-800">
                        <span className="text-cyan-700">@reason008</span>
                        <span className="text-[9px] text-slate-400">10m ago</span>
                      </div>
                      <p className="text-slate-600 mt-1">
                        Lagi seru nih di room #sampit_terindah, gabung yuk! 🥚🎁
                      </p>
                    </div>
                    <div className="p-3 text-xs">
                      <div className="flex justify-between items-center font-bold text-slate-800">
                        <span className="text-cyan-700">@neel_the_great</span>
                        <span className="text-[9px] text-slate-400">1h ago</span>
                      </div>
                      <p className="text-slate-600 mt-1">Status: Euphoria Whisper ON 🎵</p>
                    </div>
                  </div>
                </div>
              )}

              {/* 5. TAB OBROLAN DUAL CHATROOM & PM */}
              {(activeTabObject.type === 'chatroom' || activeTabObject.type === 'pm') && (
                <div className="flex-1 flex flex-col bg-white">
                  {/* Sub-Header Chat */}
                  <div className="bg-slate-100 px-3 py-2 border-b border-slate-200 flex items-center justify-between text-xs">
                    <div className="flex items-center gap-1.5 min-w-0 font-bold text-slate-800">
                      <span>{activeTabObject.title}</span>
                      <span className="text-[10px] text-emerald-600 font-mono flex items-center gap-1">
                        <span className="w-1.5 h-1.5 rounded-full bg-emerald-500 animate-pulse"></span>
                        Terhubung
                      </span>
                    </div>

                    <button
                      onClick={handleSendEgg}
                      className="bg-amber-500 hover:bg-amber-600 text-white font-bold px-3 py-1 rounded-lg text-[10px] flex items-center gap-1 shadow-sm transition active:scale-95"
                    >
                      <span>🥚 Lempar Telur</span>
                    </button>
                  </div>

                  {/* Area Obrolan */}
                  <div className="flex-1 p-3 overflow-y-auto space-y-2 bg-[#fdfcfa]">
                    {(chatHistories[activeTabId] || []).map((msg) => (
                      <div key={msg.id} className="text-xs">
                        {msg.isSystem ? (
                          <p className="text-[10px] text-amber-700 bg-amber-50 p-2 border border-amber-200 rounded-lg text-center italic font-medium">
                            {msg.text}
                          </p>
                        ) : msg.isBot ? (
                          <div className="bg-emerald-50 border border-emerald-200 rounded-xl p-2.5">
                            <span className="font-bold text-emerald-700 text-[11px]">
                              🤖 {msg.sender}:{' '}
                            </span>
                            <span className="text-slate-800">{msg.text}</span>
                          </div>
                        ) : (
                          <div className="flex flex-col">
                            <div className="flex items-baseline justify-between">
                              <span
                                className={`font-bold text-[11px] ${msg.sender === username ? 'text-orange-600' : 'text-cyan-800'}`}
                              >
                                {msg.sender}:
                              </span>
                              {msg.time && (
                                <span className="text-[9px] text-slate-400 font-mono flex items-center gap-1">
                                  {msg.time}
                                  {msg.sender === username && (
                                    <CheckCheck className="w-3 h-3 text-cyan-600" />
                                  )}
                                </span>
                              )}
                            </div>
                            <p className="text-slate-800 bg-slate-100/90 p-2.5 rounded-xl border border-slate-200/80 mt-0.5">
                              {msg.text}
                            </p>
                          </div>
                        )}
                      </div>
                    ))}
                    <div ref={chatBottomRef} />
                  </div>

                  {/* Emoticon Picker Popup */}
                  {showPicker && (
                    <div className="bg-slate-100 p-2 border-t border-slate-300 space-y-2">
                      <div className="flex gap-2 border-b border-slate-200 pb-1 text-[11px]">
                        <button
                          type="button"
                          onClick={() => setPickerTab('emoji')}
                          className={`px-2 py-0.5 rounded font-bold ${pickerTab === 'emoji' ? 'bg-cyan-600 text-white' : 'text-slate-600'}`}
                        >
                          Emoji
                        </button>
                        <button
                          type="button"
                          onClick={() => setPickerTab('sticker')}
                          className={`px-2 py-0.5 rounded font-bold ${pickerTab === 'sticker' ? 'bg-cyan-600 text-white' : 'text-slate-600'}`}
                        >
                          Stiker
                        </button>
                        <button
                          type="button"
                          onClick={() => setPickerTab('gift')}
                          className={`px-2 py-0.5 rounded font-bold ${pickerTab === 'gift' ? 'bg-cyan-600 text-white' : 'text-slate-600'}`}
                        >
                          Gift
                        </button>
                      </div>

                      <div className="grid grid-cols-4 gap-1.5">
                        {emoticons
                          .filter((e) => e.category === pickerTab)
                          .map((emo, i) => (
                            <button
                              key={i}
                              type="button"
                              onClick={() => {
                                setChatInput((prev) => prev + ' ' + emo.code);
                                setShowPicker(false);
                              }}
                              className="bg-white p-1.5 rounded-lg border border-slate-300 hover:bg-cyan-50 flex items-center justify-center gap-1 text-xs shadow-sm transition"
                            >
                              <span>{emo.symbol}</span>
                              <span className="text-[9px] text-slate-500 font-mono">
                                {emo.code}
                              </span>
                            </button>
                          ))}
                      </div>
                    </div>
                  )}

                  {/* Form Pesan */}
                  <form
                    onSubmit={handleSendMessage}
                    className="p-2 bg-slate-200 border-t border-slate-300 flex items-center gap-1.5"
                  >
                    <button
                      type="button"
                      onClick={() => setShowPicker(!showPicker)}
                      className="p-1.5 bg-white border border-slate-300 rounded-lg text-slate-700 hover:text-cyan-600 transition"
                    >
                      <Smile className="w-4 h-4" />
                    </button>

                    <input
                      type="text"
                      value={chatInput}
                      onChange={(e) => setChatInput(e.target.value)}
                      placeholder="Ketik pesan..."
                      className="flex-1 text-xs px-3 py-1.5 bg-white border border-slate-300 rounded-lg focus:outline-none focus:ring-1 focus:ring-cyan-500"
                    />

                    <button
                      type="submit"
                      className="px-4 py-1.5 bg-cyan-700 hover:bg-cyan-800 text-white font-bold rounded-lg text-xs transition"
                    >
                      Kirim
                    </button>
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
