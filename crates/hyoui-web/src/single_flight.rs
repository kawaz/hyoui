//! 同じ key の blocking 処理を gateway 内で 1 本に束ねる (single-flight)。
//!
//! session 走査や status.query は daemon への connect を伴う。browser のタブ数だけ同じ問い合わせが同時に走ると、応答の遅い daemon の listen backlog を gateway 自身が埋めてしまう。走行中の処理があれば新しい要求はそれに相乗りし、同じ結果を受け取る。
//!
//! 結果は保持しない (= 完了した瞬間に slot を空ける)。次の要求は新しく走らせる。
//!
//! Design rationale: TTL 付きキャッシュを置かない。相乗りの効き目は処理時間に比例するので、daemon が遅いほど束ねる幅が広がり、どの daemon への同時接続も gateway あたり 1 本に収まる。daemon が速い時は 1 回の処理が数 ms で終わり、backlog を圧迫しない。TTL を足しても守れるものが増えない一方、session の状態変化 (kill / stop / resume) の反映が TTL 分遅れる。

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

/// key ごとに走行中の処理を 1 本に保つ。
pub(crate) struct SingleFlight<K, V> {
    inflight: Mutex<HashMap<K, watch::Sender<Option<V>>>>,
}

/// 走行中の処理が結果を返さずに終わった (= panic)。
#[derive(Debug)]
pub(crate) struct Abandoned;

/// [`SingleFlight::join`] で登録済みの待ち。
pub(crate) struct Pending<V> {
    rx: watch::Receiver<Option<V>>,
}

impl<K, V> SingleFlight<K, V>
where
    K: Eq + Hash + Clone + Send + 'static,
    V: Clone + Send + Sync + 'static,
{
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            inflight: Mutex::new(HashMap::new()),
        })
    }

    /// `key` の処理に相乗りする。走行中のものが無ければ `work` を blocking thread で走らせる。
    ///
    /// 登録はこの呼び出しの中で終わる (= 戻った時点で、以降に完了する結果を必ず受け取れる)。`work` は要求側の future から切り離して走るので、要求が途中で捨てられても完了して slot を空ける。
    pub(crate) fn join<F>(self: &Arc<Self>, key: K, work: F) -> Pending<V>
    where
        F: FnOnce() -> V + Send + 'static,
    {
        let mut inflight = self.lock();
        if let Some(tx) = inflight.get(&key) {
            return Pending { rx: tx.subscribe() };
        }
        let (tx, rx) = watch::channel(None);
        inflight.insert(key.clone(), tx);
        drop(inflight);
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(work).await;
            // 結果を送る前に slot を外す。外した後に来た要求は新しく走らせる側に回る。
            let tx = this.lock().remove(&key);
            if let (Ok(value), Some(tx)) = (result, tx) {
                tx.send_replace(Some(value));
            }
            // panic 時は tx を drop するだけで、待ち側は Abandoned を受け取る。
        });
        Pending { rx }
    }

    /// [`join`](Self::join) して結果を待つ。
    pub(crate) async fn run<F>(self: &Arc<Self>, key: K, work: F) -> Result<V, Abandoned>
    where
        F: FnOnce() -> V + Send + 'static,
    {
        self.join(key, work).wait().await
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<K, watch::Sender<Option<V>>>> {
        // 保持中に panic する処理を置いていないので poison は起きない。起きても map 自体は壊れていない。
        self.inflight.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl<V: Clone> Pending<V> {
    pub(crate) async fn wait(mut self) -> Result<V, Abandoned> {
        match self.rx.wait_for(Option::is_some).await {
            Ok(value) => Ok(value.clone().expect("wait_for は Some でだけ返る")),
            Err(_) => Err(Abandoned),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 解放されるまで block し、実行回数を数える処理。
    fn gated_work(
        calls: &Arc<AtomicUsize>,
        gate: std::sync::mpsc::Receiver<()>,
        value: u32,
    ) -> impl FnOnce() -> u32 + Send + 'static {
        let calls = Arc::clone(calls);
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            gate.recv().unwrap();
            value
        }
    }

    #[tokio::test]
    async fn concurrent_joins_share_one_run() {
        let flight = SingleFlight::<(), u32>::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let (release, gate) = std::sync::mpsc::channel();
        let leader = flight.join((), gated_work(&calls, gate, 7));
        // 走行中に来た要求の work は走らない。
        let followers: Vec<_> = (0..5)
            .map(|_| flight.join((), || panic!("相乗りした要求の work は走らない")))
            .collect();
        release.send(()).unwrap();
        assert_eq!(leader.wait().await.unwrap(), 7);
        for f in followers {
            assert_eq!(f.wait().await.unwrap(), 7);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn finished_result_is_not_reused() {
        let flight = SingleFlight::<(), u32>::new();
        let calls = Arc::new(AtomicUsize::new(0));
        for value in [1, 2] {
            let (release, gate) = std::sync::mpsc::channel();
            release.send(()).unwrap();
            let got = flight
                .run((), gated_work(&calls, gate, value))
                .await
                .unwrap();
            assert_eq!(got, value, "完了後の要求は新しく走らせた結果を受け取る");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn different_keys_run_independently() {
        let flight = SingleFlight::<&'static str, u32>::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let (release_a, gate_a) = std::sync::mpsc::channel();
        let (release_b, gate_b) = std::sync::mpsc::channel();
        let a = flight.join("a", gated_work(&calls, gate_a, 1));
        let b = flight.join("b", gated_work(&calls, gate_b, 2));
        // b を先に完了させても a を待たない。
        release_b.send(()).unwrap();
        assert_eq!(b.wait().await.unwrap(), 2);
        release_a.send(()).unwrap();
        assert_eq!(a.wait().await.unwrap(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn panicking_work_releases_the_slot() {
        let flight = SingleFlight::<(), u32>::new();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let leader = flight.join((), move || {
            gate.recv().unwrap();
            panic!("work が落ちる")
        });
        let follower = flight.join((), || unreachable!());
        release.send(()).unwrap();
        assert!(leader.wait().await.is_err());
        assert!(follower.wait().await.is_err());
        // slot が空いているので次の要求は走る。
        assert_eq!(flight.run((), || 3).await.unwrap(), 3);
    }
}
