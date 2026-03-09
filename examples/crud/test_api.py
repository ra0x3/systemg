import sys
import time

import httpx


def test_crud_api():
    base_url = "http://localhost:8888"

    with httpx.Client(base_url=base_url) as client:
        print("Testing CRUD API Endpoints\n")

        print("1. Testing health check...")
        response = client.get("/")
        assert response.status_code == 200
        print(f"   OK: Health check: {response.json()['status']}")

        print("\n2. Creating a new todo...")
        todo_data = {
            "title": "Test systemg integration",
            "description": "Verify that systemg can manage FastAPI services",
            "is_completed": False
        }
        response = client.post("/todos", json=todo_data)
        assert response.status_code == 200
        created_todo = response.json()
        todo_id = created_todo["id"]
        print(f"   OK: Created todo with ID: {todo_id}")

        print("\n3. Reading all todos...")
        response = client.get("/todos")
        assert response.status_code == 200
        todos = response.json()
        assert len(todos) > 0
        print(f"   OK: Found {len(todos)} todo(s)")

        print("\n4. Reading specific todo...")
        response = client.get(f"/todos/{todo_id}")
        assert response.status_code == 200
        todo = response.json()
        assert todo["id"] == todo_id
        print(f"   OK: Retrieved todo: '{todo['title']}'")

        print("\n5. Updating todo...")
        update_data = {
            "title": "Test systemg integration",
            "description": "Successfully verified systemg with FastAPI!",
            "is_completed": True
        }
        response = client.put(f"/todos/{todo_id}", json=update_data)
        assert response.status_code == 200
        updated = response.json()
        assert updated["is_completed"] == True
        print(f"   OK: Updated todo - completed: {updated['is_completed']}")

        print("\n6. Testing chaos endpoint (expect some failures)...")
        success_count = 0
        failure_count = 0
        for i in range(10):
            try:
                response = client.get("/chaos", timeout=2.0)
                if response.status_code == 200:
                    success_count += 1
                    print(f"   Attempt {i+1}: Success")
                else:
                    failure_count += 1
                    print(f"   Attempt {i+1}: Failed (status {response.status_code})")
            except httpx.RequestError:
                failure_count += 1
                print(f"   Attempt {i+1}: Request error")
            time.sleep(0.5)

        print(f"   Chaos results: {success_count} successes, {failure_count} failures")
        print(f"   This demonstrates systemg's recovery capabilities")

        print("\n7. Deleting todo...")
        response = client.delete(f"/todos/{todo_id}")
        assert response.status_code == 200
        print(f"   OK: Deleted todo {todo_id}")

        print("\n8. Verifying deletion...")
        response = client.get(f"/todos/{todo_id}")
        assert response.status_code == 404
        print(f"   OK: Confirmed todo {todo_id} no longer exists")

        print("\nAll tests passed successfully!")
        print("The FastAPI CRUD service is working perfectly with systemg")


if __name__ == "__main__":
    try:
        test_crud_api()
    except httpx.ConnectError:
        print("ERROR: Could not connect to API at http://localhost:8888")
        print("Make sure to start the service with: sysg start")
        sys.exit(1)
    except AssertionError as e:
        print(f"ERROR: Test assertion failed: {e}")
        sys.exit(1)
    except Exception as e:
        print(f"ERROR: Unexpected error: {e}")
        sys.exit(1)