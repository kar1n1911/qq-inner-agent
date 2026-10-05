mod affect_support;
use qq_inner_core::affect::{self,Dimension::*};
#[test] fn decay_and_read_writeback() {
 assert_eq!(affect::decay(1.,0.,10.,10.),0.5);
 assert_eq!(affect::decay(-1.,0.5,10.,10.),-0.25);
 assert_eq!(affect::decay(1.,0.,-1.,10.),1.);
 let s=affect_support::store();
 affect::update(&s,"a","person:u",Affinity,Affinity,1.,1.,"1",100.).unwrap();
 assert!((affect::read(&s,"a","person:u",Affinity,100.+7.*86400.).unwrap()-0.05).abs()<1e-12);
 assert_eq!(affect::read(&s,"b","person:u",Affinity,100.).unwrap(),0.);
 affect::rate(&s,"a","1",-0.8,0.9,1.,100.).unwrap();
 assert_eq!(s.rows("SELECT mood,agreement FROM message_ratings",[]).unwrap()[0]["agreement"],0.9);
 assert!(affect::rate(&s,"a","2",0.,0.,1.,100.).is_err());
 assert!(affect::update(&s,"b","person:u",Affinity,Affinity,1.,1.,"1",100.).is_err());
 affect::reset(&s,"a","person:u").unwrap();
 assert_eq!(affect::read(&s,"a","person:u",Affinity,100.).unwrap(),0.);
 assert!(s.rows("SELECT * FROM message_ratings",[]).unwrap().is_empty());
}
