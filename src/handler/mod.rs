use crate::database::Database;
use async_trait::async_trait;
use cached::SizedCache;
use std::sync::Arc;
use tokio::sync::Mutex;
use walle_core::{
    action::{
        DeleteMessageContent, GetLatestEventsContent, GroupIdContent, IdsContent,
        SendMessageContent, SetGroupNameContent, UserIdContent,
    },
    impls::OneBot,
    resp::{
        GroupInfoContent, SendMessageRespContent, StatusContent, UserInfoContent, VersionContent,
    },
    Action, ActionHandler, Event, ExtendedMap, MessageContent, MessageEventType, RespContent,
    Resps,
};

pub(crate) mod v11;

pub(crate) struct Handler(
    pub(crate) Arc<rs_qq::Client>,
    pub(crate) Arc<Mutex<SizedCache<String, Event>>>,
);

#[async_trait]
impl ActionHandler<Action, Resps, OneBot> for Handler {
    async fn handle(&self, action: Action, ob: &OneBot) -> Result<Resps, Resps> {
        match action {
            Action::GetLatestEvents(c) => self.handle(c, ob).await,
            Action::GetSupportedActions(_) => Ok(Self::get_supported_actions()),
            Action::GetStatus(_) => Ok(self.get_status()),
            Action::GetVersion(_) => Ok(Self::get_version()),

            Action::SendMessage(msg) => self.handle(msg, ob).await,
            Action::DeleteMessage(c) => self.handle(c, ob).await,

            Action::GetSelfInfo(_) => Ok(self.get_self_info().await),
            Action::GetUserInfo(c) => self.handle(c, ob).await,
            Action::GetFriendList(_) => Ok(self.get_friend_list().await),

            Action::GetGroupInfo(c) => self.handle(c, ob).await,
            Action::GetGroupList(_) => Ok(self.get_group_list().await),
            Action::GetGroupMemberList(c) => self.get_group_member_list(c).await,
            Action::GetGroupMemberInfo(c) => self.get_group_member_info(c).await,
            Action::SetGroupName(c) => self.handle(c, ob).await,
            _ => Err(Resps::unsupported_action()),
        }
    }
}

#[async_trait]
impl ActionHandler<GetLatestEventsContent, Resps, OneBot> for Handler {
    async fn handle(&self, c: GetLatestEventsContent, _ob: &OneBot) -> Result<Resps, Resps> {
        let events = self
            .1
            .lock()
            .await
            .value_order()
            .take(c.limit as usize)
            .cloned()
            .collect::<Vec<_>>();
        Ok(Resps::success(events.into()))
    }
}

#[async_trait]
impl ActionHandler<SendMessageContent, Resps, OneBot> for Handler {
    async fn handle(&self, content: SendMessageContent, ob: &OneBot) -> Result<Resps, Resps> {
        if &content.detail_type == "group" {
            let group_id = content.group_id.ok_or(Resps::bad_param())?;
            let receipt = self
                .0
                .send_group_message(
                    group_id.parse().map_err(|_| Resps::bad_param())?,
                    crate::parse::msg_seg_vec2msg_chain(content.message.clone()),
                )
                .await
                .map_err(|_| Resps::platform_error())?;
            let event = ob
                .new_event(
                    MessageContent::new_group_message_content(
                        content.message,
                        receipt.seqs[0].to_string(),
                        ob.self_id.read().await.clone(),
                        group_id,
                        [
                            ("seqs".to_string(), receipt.seqs.into()),
                            ("rands".to_string(), receipt.rands.into()),
                        ]
                        .into(),
                    )
                    .into(),
                    receipt.time as f64,
                )
                .await;
            crate::SLED_DB.insert_message_event(&event);
            Ok(Resps::success(
                SendMessageRespContent {
                    message_id: event.id,
                    time: event.time as u64,
                }
                .into(),
            ))
        } else if &content.detail_type == "private" {
            let target_id = content.user_id.ok_or(Resps::bad_param())?;
            let receipt = self
                .0
                .send_private_message(
                    target_id.parse().map_err(|_| Resps::bad_param())?,
                    crate::parse::msg_seg_vec2msg_chain(content.message.clone()),
                )
                .await
                .map_err(|_| Resps::platform_error())?;
            let event = ob
                .new_event(
                    MessageContent::new_private_message_content(
                        content.message,
                        receipt.seqs[0].to_string(),
                        ob.self_id().await,
                        [
                            ("seqs".to_string(), receipt.seqs.into()),
                            ("rands".to_string(), receipt.rands.into()),
                        ]
                        .into(),
                    )
                    .into(),
                    receipt.time as f64,
                )
                .await;
            crate::SLED_DB.insert_message_event(&event);
            Ok(Resps::success(
                SendMessageRespContent {
                    message_id: event.id,
                    time: event.time as u64,
                }
                .into(),
            ))
        } else {
            Err(Resps::unsupported_action())
        }
    }
}

#[async_trait]
impl ActionHandler<UserIdContent, Resps, OneBot> for Handler {
    async fn handle(&self, action: UserIdContent, _ob: &OneBot) -> Result<Resps, Resps> {
        let user_id: i64 = action.user_id.parse().map_err(|_| Resps::bad_param())?;
        let info = self
            .0
            .find_friend(user_id)
            .await
            .ok_or(Resps::empty_fail(35001, "未找到该好友".to_owned()))?;
        Ok(Resps::success(
            UserInfoContent {
                user_id: info.uin.to_string(),
                nickname: info.nick.to_string(),
            }
            .into(),
        ))
    }
}

#[async_trait]
impl ActionHandler<GroupIdContent, Resps, OneBot> for Handler {
    async fn handle(&self, action: GroupIdContent, _ob: &OneBot) -> Result<Resps, Resps> {
        let group_id: i64 = action.group_id.parse().map_err(|_| Resps::bad_param())?;
        let info = self
            .0
            .find_group(group_id, true)
            .await
            .ok_or(Resps::empty_fail(35001, "未找到该群".to_owned()))?;
        Ok(Resps::success(
            GroupInfoContent {
                group_id: info.info.uin.to_string(),
                group_name: info.info.name.to_string(),
            }
            .into(),
        ))
    }
}

#[async_trait]
impl ActionHandler<DeleteMessageContent, Resps, OneBot> for Handler {
    async fn handle(&self, action: DeleteMessageContent, _ob: &OneBot) -> Result<Resps, Resps> {
        fn get_vec_i32(map: &mut ExtendedMap, key: &str) -> Vec<i32> {
            map.remove(key)
                .unwrap()
                .downcast_list()
                .unwrap()
                .into_iter()
                .map(|v| v.downcast_int().unwrap() as i32)
                .collect()
        }

        if let Some(mut m) = crate::SLED_DB.get_message_event(&action.message_id) {
            if let Ok(_) = match m.content.ty {
                MessageEventType::Private => {
                    self.0
                        .recall_private_message(
                            m.content.user_id.parse().unwrap(),
                            m.time as i64,
                            get_vec_i32(&mut m.content.extra, "seqs"),
                            get_vec_i32(&mut m.content.extra, "rands"),
                        )
                        .await
                }
                MessageEventType::Group { group_id } => {
                    self.0
                        .recall_group_message(
                            group_id.parse().unwrap(),
                            get_vec_i32(&mut m.content.extra, "seqs"),
                            get_vec_i32(&mut m.content.extra, "rands"),
                        )
                        .await
                }
            } {
                Ok(Resps::empty_success())
            } else {
                Err(Resps::platform_error())
            }
        } else {
            Err(Resps::empty_fail(35001, "未找到该消息".to_owned()))
        }
    }
}

#[async_trait]
impl ActionHandler<SetGroupNameContent, Resps, OneBot> for Handler {
    async fn handle(&self, c: SetGroupNameContent, _ob: &OneBot) -> Result<Resps, Resps> {
        match self
            .0
            .update_group_name(
                c.group_id.parse().map_err(|_| Resps::bad_param())?,
                c.group_name,
            )
            .await
        {
            Ok(_) => Ok(Resps::empty_success()),
            Err(_) => Err(Resps::platform_error()),
        }
    }
}

impl Handler {
    fn get_supported_actions() -> Resps {
        Resps::success(RespContent::SupportActions(vec![
            "get_latest_events".into(),
            "get_supported_actions".into(),
            "get_status".into(),
            "get_version".into(),
            "send_message".into(),
            "delete_message".into(),
            "get_self_info".into(),
            "get_user_info".into(),
            "get_friend_list".into(),
            "get_group_info".into(),
            "get_group_list".into(),
            "get_group_member_list".into(),
            "get_group_member_info".into(),
        ]))
    }
    fn get_version() -> Resps {
        Resps::success(
            VersionContent {
                r#impl: crate::WALLE_Q.to_string(),
                platform: "qq".to_string(),
                version: crate::VERSION.to_string(),
                onebot_version: OneBot::onebot_version().to_string(),
            }
            .into(),
        )
    }
    fn get_status(&self) -> Resps {
        Resps::success(
            StatusContent {
                good: true,
                online: self.0.online.load(std::sync::atomic::Ordering::Relaxed),
            }
            .into(),
        )
    }
    async fn get_self_info(&self) -> Resps {
        Resps::success(
            UserInfoContent {
                user_id: self.0.uin().await.to_string(),
                nickname: self.0.account_info.read().await.nickname.clone(),
            }
            .into(),
        )
    }
    async fn get_friend_list(&self) -> Resps {
        Resps::success(
            self.0
                .friends
                .read()
                .await
                .iter()
                .map(|i| UserInfoContent {
                    user_id: i.0.to_string(),
                    nickname: i.1.nick.to_string(),
                })
                .collect::<Vec<_>>()
                .into(),
        )
    }
    async fn get_group_list(&self) -> Resps {
        Resps::success(
            self.0
                .groups
                .read()
                .await
                .iter()
                .map(|i| GroupInfoContent {
                    group_id: i.0.to_string(),
                    group_name: i.1.info.name.clone(),
                })
                .collect::<Vec<_>>()
                .into(),
        )
    }
    async fn get_group_member_list(&self, group_id: GroupIdContent) -> Result<Resps, Resps> {
        let group_id: i64 = group_id.group_id.parse().map_err(|_| Resps::bad_param())?;
        let group = self
            .0
            .find_group(group_id, true)
            .await
            .ok_or(Resps::empty_fail(35001, "未找到该群".to_owned()))?;
        let v = group
            .members
            .read()
            .await
            .iter()
            .map(|i| UserInfoContent {
                user_id: i.uin.to_string(),
                nickname: i.nickname.clone(),
            })
            .collect::<Vec<_>>();
        Ok(Resps::success(v.into()))
    }
    async fn get_group_member_info(&self, ids: IdsContent) -> Result<Resps, Resps> {
        let group_id: i64 = ids.group_id.parse().map_err(|_| Resps::bad_param())?;
        let uin: i64 = ids.user_id.parse().map_err(|_| Resps::bad_param())?;
        let group = self
            .0
            .find_group(group_id, true)
            .await
            .ok_or(Resps::empty_fail(35001, "未找到该群".to_owned()))?;
        let list = group.members.read().await;
        let v: Vec<_> = list.iter().filter(|i| i.uin == uin).collect();
        if v.is_empty() {
            return Err(Resps::empty_fail(35001, "未找到该群成员".to_owned()));
        } else {
            Ok(Resps::success(
                UserInfoContent {
                    user_id: v[0].uin.to_string(),
                    nickname: v[0].nickname.clone(),
                }
                .into(),
            ))
        }
    }
}