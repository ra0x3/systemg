import random
from datetime import datetime, timezone
from typing import Dict, Optional

from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field

app = FastAPI(title="CRUD API", version="0.1.0")

class Todo(BaseModel):
    title: str = Field(..., min_length=1, max_length=200)
    description: str = Field(..., min_length=1, max_length=1000)
    id: Optional[int] = Field(default=None)
    timestamp: Optional[datetime] = Field(default=None)
    is_completed: bool = Field(default=False)

todos_db: Dict[int, Todo] = {}
next_id = 1

@app.get("/")
async def root():
    return {"status": "healthy", "timestamp": datetime.now(timezone.utc).isoformat()}

@app.post("/todos", response_model=Todo)
async def create_todo(todo: Todo) -> Todo:
    global next_id

    todo.id = next_id
    todo.timestamp = datetime.now(timezone.utc)
    next_id += 1

    todos_db[todo.id] = todo
    return todo

@app.get("/todos", response_model=list[Todo])
async def read_todos() -> list[Todo]:
    return list(todos_db.values())

@app.get("/todos/{todo_id}", response_model=Todo)
async def read_todo(todo_id: int) -> Todo:
    if todo_id not in todos_db:
        raise HTTPException(status_code=404, detail=f"Todo with id {todo_id} not found")
    return todos_db[todo_id]

@app.put("/todos/{todo_id}", response_model=Todo)
async def update_todo(todo_id: int, todo_update: Todo) -> Todo:
    if todo_id not in todos_db:
        raise HTTPException(status_code=404, detail=f"Todo with id {todo_id} not found")

    existing_todo = todos_db[todo_id]
    existing_todo.title = todo_update.title
    existing_todo.description = todo_update.description
    existing_todo.is_completed = todo_update.is_completed

    return existing_todo

@app.delete("/todos/{todo_id}")
async def delete_todo(todo_id: int):
    if todo_id not in todos_db:
        raise HTTPException(status_code=404, detail=f"Todo with id {todo_id} not found")

    del todos_db[todo_id]
    return {"message": f"Todo {todo_id} deleted successfully"}

@app.get("/chaos")
async def chaos_endpoint():
    if random.random() < 0.7:
        raise HTTPException(
            status_code=500,
            detail="Chaos monkey struck! Random failure to test recovery."
        )
    return {"message": "Lucky you! The chaos monkey was sleeping."}

if __name__ == "__main__":
    import uvicorn
    uvicorn.run(app, host="0.0.0.0", port=8888)