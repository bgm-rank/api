#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// 收藏统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub wish: i32,    // 想看
    pub collect: i32, // 看过
    pub doing: i32,   // 在看
    pub on_hold: i32, // 搁置
    pub dropped: i32, // 抛弃
}

// 剧集信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub id: i32,
    #[serde(rename = "type")]
    pub _type: i32, // 本篇=0 特别篇=1 OP=2 ED=3 预告/宣传/广告=4 MAD=5 其他=6
    pub name: Option<String>,          // 剧集名称
    pub name_cn: Option<String>,       // 剧集中文名称
    pub sort: Option<f64>,             // 同类条目的排序和集数
    pub ep: Option<f64>,               // 条目内的集数, 从1开始。非本篇剧集的此字段无意义
    pub airdate: Option<String>,       // 播出日期 YYYY-MM-DD 格式
    pub comment: Option<i32>,          // 评论
    pub duration: Option<String>,      // 时长
    pub desc: Option<String>,          // 简介
    pub disc: Option<i32>,             // 音乐曲目的碟片数
    pub duration_seconds: Option<i32>, // 时长秒数
    pub subject_id: Option<i32>,       // 条目ID
}

// 错误详情
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub title: Option<String>,       // 错误标题
    pub description: Option<String>, // 错误描述
    pub details: Option<String>,     // 错误详情
}

// 图片信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Images {
    pub large: Option<String>,  // 大图
    pub common: Option<String>, // 通用图
    pub medium: Option<String>, // 中图
    pub small: Option<String>,  // 小图
    pub grid: Option<String>,   // 网格图
}

// 信息框项目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfoboxItem {
    pub key: Option<String>,              // 键
    pub value: Option<serde_json::Value>, // 值（可能是字符串或数组）
}

// 索引条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexSubject {
    pub id: i32,
    #[serde(rename = "type")]
    pub _type: Option<i32>, // 1=book, 2=anime, 3=music, 4=game, 6=real
    pub name: Option<String>,              // 条目名称
    pub name_cn: Option<String>,           // 条目中文名称
    pub comment: Option<String>,           // 评论
    pub images: Option<Images>,            // 图片
    pub infobox: Option<Vec<InfoboxItem>>, // 信息框
    pub date: Option<String>,              // 上映日期 YYYY-MM-DD 格式
    pub added_at: DateTime<Utc>,           // 创建时间
}

// 分页剧集数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PagedEpisode {
    pub total: i32,
    pub limit: i32,
    pub offset: i32,
    pub data: Vec<Episode>,
}

// 分页索引条目数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PagedIndexSubject {
    pub total: i32,
    pub limit: i32,
    pub offset: i32,
    pub data: Vec<IndexSubject>,
}

// 评分信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rating {
    pub rank: Option<i32>,                   // 排名
    pub total: Option<i32>,                  // 总评分人数
    pub count: Option<HashMap<String, i32>>, // 评分人数分布 (评分 -> 人数)
    pub score: Option<f64>,                  // 评分
}

// 标签
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub name: Option<String>, // 标签名称
    pub count: Option<i32>,   // 标签数量
}

// 条目信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subject {
    pub id: i32,
    #[serde(rename = "type")]
    pub _type: i32, // 1=book, 2=anime, 3=music, 4=game, 6=real
    pub name: Option<String>,              // 条目名称
    pub name_cn: Option<String>,           // 条目中文名称
    pub summary: Option<String>,           // 简介
    pub series: Option<bool>,              // 是否为系列
    pub nsfw: Option<bool>,                // 是否包含NSFW内容
    pub locked: Option<bool>,              // 是否锁定
    pub date: Option<String>,              // 上映日期 YYYY-MM-DD 格式
    pub platform: Option<String>,          // 播放平台 TV, Web, 欧美剧, DLC...
    pub images: Option<Images>,            // 图片
    pub infobox: Option<Vec<InfoboxItem>>, // 信息框
    pub volumes: Option<i32>,              // 卷数
    pub eps: Option<i32>,                  // 集数
    pub total_episodes: Option<i32>,       // 总集数
    pub rating: Option<Rating>,            // 评分
    pub collection: Option<Collection>,    // 收藏
    pub meta_tags: Option<Vec<String>>,    // 元标签
    pub tags: Option<Vec<Tag>>,            // 标签
}

// 搜索筛选器
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchFilter {
    #[serde(rename = "type")]
    pub _type: Option<Vec<i32>>, // 条目类型
    pub meta_tags: Option<Vec<String>>, // 元标签
    pub tag: Option<Vec<String>>,       // 标签
    pub air_date: Option<Vec<String>>,  // 播出日期
    pub rating: Option<Vec<String>>,    // 评分
    pub rank: Option<Vec<String>>,      // 排名
    pub nsfw: Option<bool>,             // 是否包含NSFW内容
}

impl SearchFilter {
    /// 根据条目类型创建筛选器
    pub fn from_type(subject_type: i32) -> Self {
        Self {
            _type: Some(vec![subject_type]),
            meta_tags: None,
            tag: None,
            air_date: None,
            rating: None,
            rank: None,
            nsfw: None,
        }
    }
}

// 搜索请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub keyword: String, // 搜索关键字
    #[serde(default = "default_sort")]
    pub sort: String, // 排序规则：match=匹配度, heat=收藏人数, rank=排名, score=评分
    pub filter: Option<SearchFilter>, // 筛选条件
}

fn default_sort() -> String {
    "match".to_string()
}

impl SearchRequest {
    /// 创建简单的关键字搜索请求
    pub fn new(keyword: impl Into<String>) -> Self {
        Self {
            keyword: keyword.into(),
            sort: "match".to_string(),
            filter: None,
        }
    }

    /// 设置排序方式
    pub fn with_sort(mut self, sort: impl Into<String>) -> Self {
        self.sort = sort.into();
        self
    }

    /// 设置筛选条件
    pub fn with_filter(mut self, filter: SearchFilter) -> Self {
        self.filter = Some(filter);
        self
    }
}

// 分页条目数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PagedSubject {
    pub total: i32,
    pub limit: i32,
    pub offset: i32,
    pub data: Vec<Subject>,
}

// 目录统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stat {
    pub comments: i32, // 评论总数
    pub collects: i32, // 收藏总数
}

// 创建者信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Creator {
    pub username: String, // 用户名
    pub nickname: String, // 昵称
}

// 目录信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub id: i32,                           // 目录ID
    pub title: Option<String>,             // 目录标题
    pub desc: Option<String>,              // 目录描述
    pub total: Option<i32>,                // 收录条目总数
    pub stat: Option<Stat>,                // 目录评论及收藏数
    pub created_at: Option<DateTime<Utc>>, // 创建时间
    pub updated_at: Option<DateTime<Utc>>, // 更新时间
    pub creator: Option<Creator>,          // 创建者信息
    #[deprecated]
    pub ban: Option<bool>, // 已弃用，始终为false
    pub nsfw: Option<bool>,                // 目录是否包括 nsfw 条目
}

// 目录基本信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexBasicInfo {
    pub title: Option<String>,       // 目录标题
    pub description: Option<String>, // 目录描述
}

// 向目录添加条目请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddSubjectToIndexRequest {
    pub subject_id: i32, // 条目ID
    pub sort: i32,       // 排序
    pub comment: String, // 评论
}
