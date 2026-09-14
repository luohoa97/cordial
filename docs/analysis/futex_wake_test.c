// Standalone test of cordial's exact bionic-FUTEX -> FreeBSD _umtx_op mapping.
// Does WAKE_PRIVATE actually wake a WAIT_UINT_PRIVATE waiter? Timed and untimed.
#include <stdio.h>
#include <stdint.h>
#include <pthread.h>
#include <errno.h>
#include <time.h>
#include <unistd.h>
extern int _umtx_op(void *obj, int op, unsigned long val, void *uaddr, void *uaddr2);
#define WAIT_UINT_PRIVATE 15
#define WAKE_PRIVATE 16
#define ABSTIME 1
#define CLOCK_MONO 4
struct fbsd_umtx_time { struct timespec _timeout; uint32_t _flags; uint32_t _clockid; };

static volatile unsigned int word = 0;
static volatile int woke = 0;

// mirrors do_futex WAIT_BITSET (timed, absolute monotonic deadline)
static long fwait_bitset(void *uaddr, unsigned int val, const struct timespec *to){
  struct fbsd_umtx_time ut; void *tp=0; unsigned long tsz=0;
  if(to){ ut._timeout=*to; ut._flags=ABSTIME; ut._clockid=CLOCK_MONO; tp=&ut; tsz=sizeof(ut);}
  int r=_umtx_op(uaddr, WAIT_UINT_PRIVATE, val, (void*)tsz, tp);
  return r==0?0:-errno;
}
static long wake_priv(void *uaddr, unsigned int n){
  int r=_umtx_op(uaddr, WAKE_PRIVATE, n, 0, 0);
  return r==0?0:-errno;
}
static void* waiter(void* arg){
  (void)arg;
  // park while word==0, with a 5s absolute deadline (like the engine's timed wait)
  struct timespec now; clock_gettime(CLOCK_MONOTONIC,&now); now.tv_sec+=5;
  printf("[waiter] parking on WAIT_UINT_PRIVATE(word==0), 5s deadline\n"); fflush(stdout);
  long r=fwait_bitset((void*)&word, 0, &now);
  printf("[waiter] woke: r=%ld (0=woken/value-changed, -60=ETIMEDOUT) word=%u\n", r, word); fflush(stdout);
  woke=1;
  return 0;
}
int main(){
  pthread_t t; pthread_create(&t,0,waiter,0);
  usleep(500000); // let the waiter park
  printf("[main] changing word=1 and WAKE_PRIVATE\n"); fflush(stdout);
  word=1;
  long w=wake_priv((void*)&word, 1);
  printf("[main] wake returned %ld\n", w); fflush(stdout);
  usleep(500000);
  printf("[result] %s\n", woke ? "WAKE REACHED THE WAITER (mapping OK)" : "WAITER STILL PARKED (wake lost!) -- will time out at 5s");
  pthread_join(t,0);
  return 0;
}
